use crate::{
	data::{self, Key, APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, STATE_CF},
	network::p2p::{DatabaseIter, Entry, Iter, Record},
};
use codec::{Decode, Encode};
use color_eyre::eyre::{eyre, Context, Result};
use libp2p::kad;
use rocksdb::{
	ColumnFamilyDescriptor, DBIteratorWithThreadMode, DBWithThreadMode, IteratorMode,
	MultiThreaded, Options,
};
use serde::{Deserialize, Serialize};
use std::{iter, sync::Arc};

use super::{FINALITY_SYNC_CHECKPOINT_KEY, KADEMLIA_STORE_CF};

#[derive(Clone)]
pub struct RocksDB {
	db: Arc<rocksdb::DB>,
}

const CF_LIST: [&str; 5] = [
	CONFIDENCE_FACTOR_CF,
	BLOCK_HEADER_CF,
	APP_DATA_CF,
	STATE_CF,
	KADEMLIA_STORE_CF,
];

impl RocksDB {
	pub fn open(path: &str) -> Result<RocksDB> {
		let cf_opts = CF_LIST
			.iter()
			.map(|&cf| ColumnFamilyDescriptor::new(cf, Options::default()))
			.collect::<Vec<_>>();

		let mut db_opts = Options::default();
		db_opts.create_if_missing(true);
		db_opts.create_missing_column_families(true);

		let db = rocksdb::DB::open_cf_descriptors(&db_opts, path, cf_opts)?;
		Ok(RocksDB { db: Arc::new(db) })
	}
}

type RocksKey = (Option<&'static str>, Vec<u8>);

impl From<Key> for (Option<&'static str>, Vec<u8>) {
	fn from(key: Key) -> Self {
		match key {
			Key::AppData(app_id, block_number) => (
				Some(APP_DATA_CF),
				format!("{app_id}:{block_number}").into_bytes(),
			),
			Key::BlockHeader(block_number) => {
				(Some(BLOCK_HEADER_CF), block_number.to_be_bytes().to_vec())
			},
			Key::VerifiedCellCount(block_number) => (
				Some(CONFIDENCE_FACTOR_CF),
				block_number.to_be_bytes().to_vec(),
			),
			Key::FinalitySyncCheckpoint => (
				Some(STATE_CF),
				FINALITY_SYNC_CHECKPOINT_KEY.as_bytes().to_vec(),
			),
			Key::KademliaRecord(key) => (Some(KADEMLIA_STORE_CF), key),
		}
	}
}

impl data::Database for RocksDB {
	type Key = RocksKey;

	fn put<T>(&self, key: Key, value: T) -> Result<()>
	where
		T: Serialize + Encode,
	{
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, put the key in that partition
		let Some(cf) = column_family else {
			// else, just put it in the default partition
			return self
				.db
				.put(key, <T>::encode(&value))
				.wrap_err("Put operation failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.put_cf(&cf_handle, key, <T>::encode(&value))
			.wrap_err("Put operation with Column Family failed on RocksDB")
	}

	fn get<T>(&self, key: Key) -> Result<Option<T>>
	where
		T: for<'a> Deserialize<'a> + Decode,
	{
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, get the key from that partition
		let Some(cf) = column_family else {
			// else, just get it from the default partition
			return self
				.db
				.get(key)?
				.map(|value| <T>::decode(&mut &value[..]).wrap_err("Failed decoding the app data."))
				.transpose()
				.wrap_err("Get operation failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;

		self.db
			.get_cf(&cf_handle, key)?
			.map(|value| <T>::decode(&mut &value[..]).wrap_err("Failed decoding the app data."))
			.transpose()
			.wrap_err("Get operation with Column Family failed on RocksDB")
	}

	fn delete(&self, key: Key) -> Result<()> {
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, delete the key from that partition
		let Some(cf) = column_family else {
			// else, just delete it from the default partition
			return self
				.db
				.delete(key)
				.wrap_err("Delete operation failed on RocksDB");
		};
		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.delete_cf(&cf_handle, key)
			.wrap_err("Delete operation with Column Family failed on RocksDB")
	}
}

fn to_kad_record(result: Result<(Box<[u8]>, Box<[u8]>), rocksdb::Error>) -> kad::Record {
	// TODO: Error handling
	let (key, value) = result.expect("Expected value, got error");
	let decode_result = Record::decode(&mut &value[..]);
	let record = decode_result.expect("Expected valid encoded record, got invalid");
	Entry(key.to_vec(), record).into()
}

impl Iter for RocksDB {
	type Iterator = DatabaseIter<
		iter::Map<
			DBIteratorWithThreadMode<'static, DBWithThreadMode<MultiThreaded>>,
			fn(Result<(Box<[u8]>, Box<[u8]>), rocksdb::Error>) -> kad::Record,
		>,
	>;

	fn iter(&self) -> Self::Iterator {
		let inner = self
			.db
			.full_iterator(IteratorMode::Start)
			.map(to_kad_record as fn(_) -> _);
		DatabaseIter { inner }
	}
}
