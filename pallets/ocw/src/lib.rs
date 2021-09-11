#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

// 废止Regex和lazy_static

// #[macro_use] extern crate lazy_static;
// extern crate regex;

#[frame_support::pallet]
pub mod pallet {
	//! A demonstration of an offchain worker that sends onchain callbacks
	use core::{convert::TryInto, fmt};
	use parity_scale_codec::{Decode, Encode};
	use frame_support::pallet_prelude::*;
	use frame_system::{
		pallet_prelude::*,
		offchain::{
			AppCrypto, CreateSignedTransaction, SendSignedTransaction, SendUnsignedTransaction,
			SignedPayload, Signer, SigningTypes, SubmitTransaction,
		},
	};
	use sp_core::{crypto::KeyTypeId};
	use sp_arithmetic::per_things::Permill;
	use sp_runtime::{
		offchain as rt_offchain,
		traits::{
			BlockNumberProvider
		},
		offchain::{
			storage::StorageValueRef,
			storage_lock::{BlockAndTime, StorageLock},
		},
		transaction_validity::{
			InvalidTransaction, TransactionSource, TransactionValidity, ValidTransaction,
		},
		RuntimeDebug,
	};
	use sp_std::{collections::vec_deque::VecDeque, prelude::*, str};

	use serde::{Deserialize, Deserializer};

	/// Defines application identifier for crypto keys of this module.
	///
	/// Every module that deals with signatures needs to declare its unique identifier for
	/// its crypto keys.
	/// When an offchain worker is signing transactions it's going to request keys from type
	/// `KeyTypeId` via the keystore to sign the transaction.
	/// The keys can be inserted manually via RPC (see `author_insertKey`).
	pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"demo");
	const NUM_VEC_LEN: usize = 10;
	/// The type to sign and send transactions.
	const UNSIGNED_TXS_PRIORITY: u64 = 100;

	// We are fetching information from the github public API about organization`substrate-developer-hub`.
	const HTTP_REMOTE_REQUEST: &str = "https://api.github.com/orgs/substrate-developer-hub";
	const HTTP_REMOTE_REQUEST_COINCAP: &str = "https://api.coincap.io/v2/assets/polkadot";
	const HTTP_HEADER_USER_AGENT: &str = "jimmychu0807";

	const FETCH_TIMEOUT_PERIOD: u64 = 3000; // in milli-seconds
	const LOCK_TIMEOUT_EXPIRATION: u64 = FETCH_TIMEOUT_PERIOD + 1000; // in milli-seconds
	const LOCK_BLOCK_EXPIRATION: u32 = 3; // in block number

	/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrapper.
	/// We can utilize the supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
	/// them with the pallet-specific identifier.
	pub mod crypto {
		use crate::KEY_TYPE;
		use sp_core::sr25519::Signature as Sr25519Signature;
		use sp_runtime::app_crypto::{app_crypto, sr25519};
		use sp_runtime::{traits::Verify, MultiSignature, MultiSigner};

		app_crypto!(sr25519, KEY_TYPE);

		pub struct TestAuthId;
		// implemented for ocw-runtime
		impl frame_system::offchain::AppCrypto<MultiSigner, MultiSignature> for TestAuthId {
			type RuntimeAppPublic = Public;
			type GenericSignature = sp_core::sr25519::Signature;
			type GenericPublic = sp_core::sr25519::Public;
		}

		// implemented for mock runtime in test
		impl frame_system::offchain::AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature>
		for TestAuthId
		{
			type RuntimeAppPublic = Public;
			type GenericSignature = sp_core::sr25519::Signature;
			type GenericPublic = sp_core::sr25519::Public;
		}
	}

	#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug)]
	pub struct Payload<Public> {
		number: u64,
		public: Public,
	}

	impl<T: SigningTypes> SignedPayload<T> for Payload<T::Public> {
		fn public(&self) -> T::Public {
			self.public.clone()
		}
	}

	#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug)]
	pub struct PricePayload<Public, BlockNumber> {
		block_number: BlockNumber,
		parsed_price: (u64, Permill),
		public: Public,
	}

	impl<T: SigningTypes> SignedPayload<T> for PricePayload<T::Public, T::BlockNumber> {
		fn public(&self) -> T::Public {
			self.public.clone()
		}
	}

	// ref: https://serde.rs/container-attrs.html#crate
	#[derive(Deserialize, Encode, Decode, Default)]
	struct GithubInfo {
		// Specify our own deserializing function to convert JSON string to vector of bytes
		#[serde(deserialize_with = "de_string_to_bytes")]
		login: Vec<u8>,
		#[serde(deserialize_with = "de_string_to_bytes")]
		blog: Vec<u8>,
		public_repos: u32,
	}

	#[derive(Debug, Deserialize, Encode, Decode, Default)]
	struct IndexingData(Vec<u8>, u64);

	pub fn de_string_to_bytes<'de, D>(de: D) -> Result<Vec<u8>, D::Error>
	where
	D: Deserializer<'de>,
	{
		let s: &str = Deserialize::deserialize(de)?;
		Ok(s.as_bytes().to_vec())
	}

	impl fmt::Debug for GithubInfo {
		// `fmt` converts the vector of bytes inside the struct back to string for
		//   more friendly display.
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
			write!(
				f,
				"{{ login: {}, blog: {}, public_repos: {} }}",
				str::from_utf8(&self.login).map_err(|_| fmt::Error)?,
				str::from_utf8(&self.blog).map_err(|_| fmt::Error)?,
				&self.public_repos
				)
		}
	}

	// 备注：PriceInfo结构目前仅有priceUsd，以后可以扩展更多的信息内容。
	#[derive(Deserialize, Encode, Decode, Default)]
	struct PriceInfo {
		#[serde(deserialize_with = "de_string_to_bytes")]
		price_vec: Vec<u8>,
	}

	impl fmt::Debug for PriceInfo {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
			write!(
				f,
				"{{ priceUsd: {} }}",
				str::from_utf8(&self.price_vec).map_err(|_| fmt::Error)?,
				)
		}
	}

	#[pallet::config]
	pub trait Config: frame_system::Config + CreateSignedTransaction<Call<Self>> {
		/// The overarching event type.
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		/// The overarching dispatch call type.
		type Call: From<Call<Self>>;
		/// The identifier type for an offchain worker.
		type AuthorityId: AppCrypto<Self::Public, Self::Signature>;
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(_);

	// The pallet's runtime storage items.
	// https://substrate.dev/docs/en/knowledgebase/runtime/storage
	#[pallet::storage]
	#[pallet::getter(fn numbers)]
	// Learn more about declaring storage items:
	// https://substrate.dev/docs/en/knowledgebase/runtime/storage#declaring-storage-items
	pub type Numbers<T> = StorageValue<_, VecDeque<u64>, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn prices)]
	pub type Prices<T> = StorageValue<_, VecDeque<(u64, Permill)>, ValueQuery>;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		NewNumber(Option<T::AccountId>, u64),
		NewPrice(Option<T::AccountId>,(u64, Permill)),
	}

	// Errors inform users that something went wrong.
	#[pallet::error]
	pub enum Error<T> {
		// Error returned when not sure which ocw function to executed
		UnknownOffchainMux,

		// Error returned when making signed transactions in off-chain worker
		NoLocalAcctForSigning,
		OffchainSignedTxError,

		// Error returned when making unsigned transactions in off-chain worker
		OffchainUnsignedTxError,

		// Error returned when making unsigned transactions with signed payloads in off-chain worker
		OffchainUnsignedTxSignedPayloadError,

		// Error returned when fetching github info
		HttpFetchingError,

		NoUTF8Body,
		ParsePriceError,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		/// Offchain Worker entry point.
		///
		/// By implementing `fn offchain_worker` you declare a new offchain worker.
		/// This function will be called when the node is fully synced and a new best block is
		/// succesfuly imported.
		/// Note that it's not guaranteed for offchain workers to run on EVERY block, there might
		/// be cases where some blocks are skipped, or for some the worker runs twice (re-orgs),
		/// so the code should be able to handle that.
		/// You can use `Local Storage` API to coordinate runs of the worker.
		fn offchain_worker(block_number: T::BlockNumber) {
			log::info!("Hello World from offchain workers!");

			// Here we are showcasing various techniques used when running off-chain workers (ocw)
			// 1. Sending signed transaction from ocw
			// 2. Sending unsigned transaction from ocw
			// 3. Sending unsigned transactions with signed payloads from ocw
			// 4. Fetching JSON via http requests in ocw
			const TX_TYPES: u32 = 5;
			let modu = block_number.try_into().map_or(TX_TYPES, |bn: usize| (bn as u32) % TX_TYPES);
			let result = match modu {
				0 => Self::offchain_signed_tx(block_number),
				1 => Self::offchain_unsigned_tx(block_number),
				2 => Self::offchain_unsigned_tx_signed_payload(block_number),
				3 => Self::fetch_github_info(),
				4 => Self::fetch_price_info(block_number),
				_ => Err(Error::<T>::UnknownOffchainMux),
			};

			if let Err(e) = result {
				log::error!("offchain_worker error: {:?}", e);
			}
		}
	}

	#[pallet::validate_unsigned]
	impl<T: Config> ValidateUnsigned for Pallet<T> {
		type Call = Call<T>;

		/// Validate unsigned call to this module.
		///
		/// By default unsigned transactions are disallowed, but implementing the validator
		/// here we make sure that some particular calls (the ones produced by offchain worker)
		/// are being whitelisted and marked as valid.
		fn validate_unsigned(_source: TransactionSource, call: &Self::Call)
		-> TransactionValidity
		{
			let valid_tx = |provide| ValidTransaction::with_tag_prefix("ocw-demo")
			.priority(UNSIGNED_TXS_PRIORITY)
			.and_provides([&provide])
			.longevity(3)
			.propagate(true)
			.build();

			match call {
				Call::submit_number_unsigned(_number) => valid_tx(b"submit_number_unsigned".to_vec()),
				Call::submit_number_unsigned_with_signed_payload(ref payload, ref signature) => {
					if !SignedPayload::<T>::verify::<T::AuthorityId>(payload, signature.clone()) {
						return InvalidTransaction::BadProof.into();
					}
					valid_tx(b"submit_number_unsigned_with_signed_payload".to_vec())
				},
				Call::submit_price_unsigned_with_signed_payload(ref price_payload, ref signature) => {
					if !SignedPayload::<T>::verify::<T::AuthorityId>(price_payload, signature.clone()) {
						return InvalidTransaction::BadProof.into();
					}
					valid_tx(b"submit_price_unsigned_with_signed_payload".to_vec())
				},
				_ => InvalidTransaction::Call.into(),
			}
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		#[pallet::weight(0)]
		pub fn submit_number_signed(origin: OriginFor<T>, number: u64) -> DispatchResult {
			let who = ensure_signed(origin)?;
			log::info!("submit_number_signed: ({}, {:?})", number, who);
			Self::append_or_replace_number(number);

			Self::deposit_event(Event::NewNumber(Some(who), number));
			Ok(())
		}

		#[pallet::weight(0)]
		pub fn submit_number_unsigned(origin: OriginFor<T>, number: u64) -> DispatchResult {
			let _ = ensure_none(origin)?;
			log::info!("submit_number_unsigned: {}", number);
			Self::append_or_replace_number(number);

			Self::deposit_event(Event::NewNumber(None, number));
			Ok(())
		}

		#[pallet::weight(0)]
		pub fn submit_number_unsigned_with_signed_payload(origin: OriginFor<T>, payload: Payload<T::Public>,
			_signature: T::Signature) -> DispatchResult
		{
			let _ = ensure_none(origin)?;
			// we don't need to verify the signature here because it has been verified in
			//   `validate_unsigned` function when sending out the unsigned tx.
			let Payload { number, public } = payload;
			log::info!("submit_number_unsigned_with_signed_payload: ({}, {:?})", number, public);
			Self::append_or_replace_number(number);

			Self::deposit_event(Event::NewNumber(None, number));
			Ok(())
		}

		#[pallet::weight(0)]
		pub fn submit_price_unsigned_with_signed_payload(
			origin: OriginFor<T>,
			price_payload: PricePayload<T::Public, T::BlockNumber>,
			_signature: T::Signature) -> DispatchResult
		{
			// 确保提交的是无签名交易
			let _ = ensure_none(origin)?;
			// we don't need to verify the signature here because it has been verified in
			//   `validate_unsigned` function when sending out the unsigned tx.
			let PricePayload { block_number: _, parsed_price, public: _ } = price_payload;
			log::info!("submit_price_unsigned_with_signed_payload");
			Self::append_or_replace_price(parsed_price);

			Self::deposit_event(Event::NewPrice(None, parsed_price));
			Ok(())
		}

	}

	impl<T: Config> Pallet<T> {
		/// Append a new number to the tail of the list, removing an element from the head if reaching
		///   the bounded length.
		fn append_or_replace_number(number: u64) {
			Numbers::<T>::mutate(|numbers| {
				if numbers.len() == NUM_VEC_LEN {
					let _ = numbers.pop_front();
				}
				numbers.push_back(number);
				log::info!("Number vector: {:?}", numbers);
			});
		}

		/// append or replace Storage Prices.
		fn append_or_replace_price(price: (u64, Permill)) {
			Prices::<T>::mutate(|prices| {
				if prices.len() == NUM_VEC_LEN {
					let _ = prices.pop_front();
				}
				prices.push_back(price);
				log::info!("Prices vector: {:?}", prices);
			});
		}

		fn fetch_price_info(block_number: T::BlockNumber) -> Result<(), Error<T>> {
			// TODO: 这是你们的功课

			// 利用 offchain worker 取出 DOT 当前对 USD 的价格，并把写到一个 Vec 的存储里，
			// 你们自己选一种方法提交回链上，并在代码注释为什么用这种方法提交回链上最好。只保留当前最近的 10 个价格，
			// 其他价格可丢弃 （就是 Vec 的长度长到 10 后，这时再插入一个值时，要先丢弃最早的那个值）。

			// 取得的价格 parse 完后，放在以下存儲：
			// pub type Prices<T> = StorageValue<_, VecDeque<(u64, Permill)>, ValueQuery>

			// 这个 http 请求可得到当前 DOT 价格：
			// [https://api.coincap.io/v2/assets/polkadot](https://api.coincap.io/v2/assets/polkadot)。

			// 从coincap获取当前DOT的USD报价，并解析为（u64, Permill)元组。
			let price = Self::fetch_price().map_err(|_| <Error<T>>::HttpFetchingError)?;

			// 利用 PriceInfo 结构，在 ocw 的 local storage 存入价格信息的Vec格式。
			let price_info: PriceInfo = PriceInfo {
				price_vec: price.clone(),
			};

			let s_info = StorageValueRef::persistent(b"offchain-demo::price-info");
			let mut lock = StorageLock::<BlockAndTime<Self>>::with_block_and_time_deadline(
				b"offchain-demo::lock", LOCK_BLOCK_EXPIRATION,
				rt_offchain::Duration::from_millis(LOCK_TIMEOUT_EXPIRATION)
				);
			if let Ok(_guard) = lock.try_lock() {
				s_info.set(&price_info);
			}

			// 将price转为&str，传入parse_price()，得到解析后的price元组，
			// 分割成整数部分和Permill小数部分。
			let price_str = str::from_utf8(&price).map_err(|_| {
				log::warn!("No UTF8 body");
				<Error<T>>::NoUTF8Body
			})?;
			let parsed_price = match Self::parse_price(&price_str) {
				Some(parsed_price) => Ok(parsed_price),
				None => {
					log::warn!("Unable to extract price from the price_str: {:?}", price_str);
					Err(<Error<T>>::ParsePriceError)
				}
			}?;

			// 使用无签名交易带有签名payload的方法，将获取到的Price提交到链上
			// 理由：
			// 1）DOT价格信息来源于Coincap公开信息，与某个特定帐户没有特别关联，采用无签名交易可以避免手续费；
			// 2）对上链数据签名，确保上链的数据能够被验证其来源。

			// 检索出一个account对PricePayload签名
			let signer = Signer::<T, T::AuthorityId>::any_account();
			if let Some((_, res)) = signer.send_unsigned_transaction(
					|acct| PricePayload { block_number, parsed_price, public: acct.public.clone() },
					Call::submit_price_unsigned_with_signed_payload
				) {
					return res.map_err(|_| {
						log::error!("Failed in offchain_unsigned_tx_signed_payload");
						<Error<T>>::OffchainUnsignedTxSignedPayloadError
					});
				}

			// The case of `None`: no account is available for sending
			log::error!("No local account available");
			Err(<Error<T>>::NoLocalAcctForSigning)
		}

		/// Fetch current price and return the result in cents.
		///
		/// TODO：以后需要逐步加入更多的Json校验功能、异常捕获等
		fn fetch_price() -> Result<Vec<u8>, Error<T>> {
			log::info!("sending request to: {}", HTTP_REMOTE_REQUEST_COINCAP);

			// Initiate an external HTTP GET request. This is using high-level wrappers from `sp_runtime`.
			let request = rt_offchain::http::Request::get(HTTP_REMOTE_REQUEST_COINCAP);

			// Keeping the offchain worker execution time reasonable, so limiting the call to be within 3s.
			let timeout = sp_io::offchain::timestamp()
				.add(rt_offchain::Duration::from_millis(FETCH_TIMEOUT_PERIOD));

			let pending = request
				.deadline(timeout) // Setting the timeout time
				.send() // Sending the request out by the host
				.map_err(|_| <Error<T>>::HttpFetchingError)?;

			let response = pending.try_wait(timeout)
				.map_err(|_| <Error<T>>::HttpFetchingError)?
				.map_err(|_| <Error<T>>::HttpFetchingError)?;

			if response.code != 200 {
				log::error!("Unexpected http request status code: {}", response.code);
				return Err(<Error<T>>::HttpFetchingError);
			}

			let body = response.body().collect::<Vec<u8>>();

			// Create a str slice from the body.
			let body_str = str::from_utf8(&body).map_err(|_| {
				log::warn!("No UTF8 body");
				<Error<T>>::NoUTF8Body
			})?;

			// 利用serde_json取得解析后的Value。
			let v: serde_json::Value = serde_json::from_str(&body_str).map_err(|_| <Error<T>>::HttpFetchingError)?;

			// 从Value中取得priceUsd字段值，并转为Vec<u8>
			let price_vec: Vec<u8> = v["data"]["priceUsd"].as_str().unwrap().as_bytes().to_vec();

			Ok(price_vec)
		}

		/// 传入从coincap上取得的USD价格的字符串，将其解析为整数u64，小数Permill格式
		fn parse_price(price_str: &str) -> Option<(u64, Permill)> {
			// expected price_str is: "33.2564125398239201"
			// 转换为bytes，便于遍历
			let price = price_str.as_bytes();

			//依据小数点，找到其位置
			let mut position = 0;
			for (i, &item) in price.iter().enumerate() {
				if item == b'.' {
					position = i;
					break;
				}
			}
			// 取得integer部分的字符串bytes
			let integer = &price[0..position];

			// 取得fraction部分的字符串bytes
			let price_len = price.len();
			let fraction = match price_len >= position+7 {
				true => &price[position+1..position+7],
				false => &price[position+1..price_len],
			};

			let integer_str = str::from_utf8(integer).unwrap();
			let fraction_str = str::from_utf8(fraction).unwrap();

			let price_integer: u64 = integer_str.parse().unwrap();
			let price_fraction = Permill::from_parts(fraction_str.parse().unwrap());

			// 返回Option<(u64, Permill)>结果
			return Some((price_integer, price_fraction))

			// --- 下列代码使用了外部crates，正则表达式。暂时废止，改用runtime库解析price_str
			/*
			// 此正则表达式，用于检验从coincap获得的priceUsd字段，并用于捕获整数部分和小数点后6位
			lazy_static! {
				// 分组<integer_part>: 整数部分
				// 分组<fraction_part>: 小数部分取6位
				static ref RE: regex::Regex = regex::Regex::new(r"(?x)
					(?P<integer_part>[[:digit:]]+)
					(\.)
					(?P<fraction_part>[[:digit:]]{6})
					([[:digit:]]*)
					").unwrap();
			}
			// 使用正则表达式捕获整数部分以及6位小数部分
			for caps in RE.captures_iter(price_str) {
				let cap_integer = &caps["integer_part"];
				let cap_fraction = &caps["fraction_part"];
				// 显示捕获结果
				log::info!("Interger is: {:?}, Fraction is: {:?}",
					 &cap_integer, &cap_fraction);
				let price = Some((
					cap_integer.parse().unwrap(),
					Permill::from_parts(cap_fraction.parse().unwrap())
				));
				return price
			}
			log::info!("parse_price() return None!");
			return None;
			*/
		}

		/// Check if we have fetched github info before. If yes, we can use the cached version
		///   stored in off-chain worker storage `storage`. If not, we fetch the remote info and
		///   write the info into the storage for future retrieval.
		fn fetch_github_info() -> Result<(), Error<T>> {
			// Create a reference to Local Storage value.
			// Since the local storage is common for all offchain workers, it's a good practice
			// to prepend our entry with the pallet name.
			let s_info = StorageValueRef::persistent(b"offchain-demo::gh-info");

			// Local storage is persisted and shared between runs of the offchain workers,
			// offchain workers may run concurrently. We can use the `mutate` function to
			// write a storage entry in an atomic fashion.
			//
			// With a similar API as `StorageValue` with the variables `get`, `set`, `mutate`.
			// We will likely want to use `mutate` to access
			// the storage comprehensively.
			//
			if let Ok(Some(gh_info)) = s_info.get::<GithubInfo>() {
				// gh-info has already been fetched. Return early.
				log::info!("cached gh-info: {:?}", gh_info);
				return Ok(());
			}

			// Since off-chain storage can be accessed by off-chain workers from multiple runs, it is important to lock
			//   it before doing heavy computations or write operations.
			//
			// There are four ways of defining a lock:
			//   1) `new` - lock with default time and block exipration
			//   2) `with_deadline` - lock with default block but custom time expiration
			//   3) `with_block_deadline` - lock with default time but custom block expiration
			//   4) `with_block_and_time_deadline` - lock with custom time and block expiration
			// Here we choose the most custom one for demonstration purpose.
			let mut lock = StorageLock::<BlockAndTime<Self>>::with_block_and_time_deadline(
				b"offchain-demo::lock", LOCK_BLOCK_EXPIRATION,
				rt_offchain::Duration::from_millis(LOCK_TIMEOUT_EXPIRATION)
				);

			// We try to acquire the lock here. If failed, we know the `fetch_n_parse` part inside is being
			//   executed by previous run of ocw, so the function just returns.
			if let Ok(_guard) = lock.try_lock() {
				match Self::fetch_n_parse() {
					Ok(gh_info) => { s_info.set(&gh_info); }
					Err(err) => { return Err(err); }
				}
			}
			Ok(())
		}

		/// Fetch from remote and deserialize the JSON to a struct
		fn fetch_n_parse() -> Result<GithubInfo, Error<T>> {
			let resp_bytes = Self::fetch_from_remote().map_err(|e| {
				log::error!("fetch_from_remote error: {:?}", e);
				<Error<T>>::HttpFetchingError
			})?;

			let resp_str = str::from_utf8(&resp_bytes).map_err(|_| <Error<T>>::HttpFetchingError)?;
			// Print out our fetched JSON string
			log::info!("{}", resp_str);

			// Deserializing JSON to struct, thanks to `serde` and `serde_derive`
			let gh_info: GithubInfo =
			serde_json::from_str(&resp_str).map_err(|_| <Error<T>>::HttpFetchingError)?;
			Ok(gh_info)
		}

		/// This function uses the `offchain::http` API to query the remote github information,
		///   and returns the JSON response as vector of bytes.
		fn fetch_from_remote() -> Result<Vec<u8>, Error<T>> {
			log::info!("sending request to: {}", HTTP_REMOTE_REQUEST);

			// Initiate an external HTTP GET request. This is using high-level wrappers from `sp_runtime`.
			let request = rt_offchain::http::Request::get(HTTP_REMOTE_REQUEST);

			// Keeping the offchain worker execution time reasonable, so limiting the call to be within 3s.
			let timeout = sp_io::offchain::timestamp()
			.add(rt_offchain::Duration::from_millis(FETCH_TIMEOUT_PERIOD));

			// For github API request, we also need to specify `user-agent` in http request header.
			//   See: https://developer.github.com/v3/#user-agent-required
			let pending = request
			.add_header("User-Agent", HTTP_HEADER_USER_AGENT)
				.deadline(timeout) // Setting the timeout time
				.send() // Sending the request out by the host
				.map_err(|_| <Error<T>>::HttpFetchingError)?;

			// By default, the http request is async from the runtime perspective. So we are asking the
			//   runtime to wait here.
			// The returning value here is a `Result` of `Result`, so we are unwrapping it twice by two `?`
			//   ref: https://substrate.dev/rustdocs/v2.0.0/sp_runtime/offchain/http/struct.PendingRequest.html#method.try_wait
			let response = pending
			.try_wait(timeout)
			.map_err(|_| <Error<T>>::HttpFetchingError)?
			.map_err(|_| <Error<T>>::HttpFetchingError)?;

			if response.code != 200 {
				log::error!("Unexpected http request status code: {}", response.code);
				return Err(<Error<T>>::HttpFetchingError);
			}

			// Next we fully read the response body and collect it to a vector of bytes.
			Ok(response.body().collect::<Vec<u8>>())
		}

		fn offchain_signed_tx(block_number: T::BlockNumber) -> Result<(), Error<T>> {
			// We retrieve a signer and check if it is valid.
			//   Since this pallet only has one key in the keystore. We use `any_account()1 to
			//   retrieve it. If there are multiple keys and we want to pinpoint it, `with_filter()` can be chained,
			let signer = Signer::<T, T::AuthorityId>::any_account();

			// Translating the current block number to number and submit it on-chain
			let number: u64 = block_number.try_into().unwrap_or(0);

			// `result` is in the type of `Option<(Account<T>, Result<(), ()>)>`. It is:
			//   - `None`: no account is available for sending transaction
			//   - `Some((account, Ok(())))`: transaction is successfully sent
			//   - `Some((account, Err(())))`: error occured when sending the transaction
			let result = signer.send_signed_transaction(|_acct|
				// This is the on-chain function
				Call::submit_number_signed(number)
				);

			// Display error if the signed tx fails.
			if let Some((acc, res)) = result {
				if res.is_err() {
					log::error!("failure: offchain_signed_tx: tx sent: {:?}", acc.id);
					return Err(<Error<T>>::OffchainSignedTxError);
				}
				// Transaction is sent successfully
				return Ok(());
			}

			// The case of `None`: no account is available for sending
			log::error!("No local account available");
			Err(<Error<T>>::NoLocalAcctForSigning)
		}

		fn offchain_unsigned_tx(block_number: T::BlockNumber) -> Result<(), Error<T>> {
			let number: u64 = block_number.try_into().unwrap_or(0);
			let call = Call::submit_number_unsigned(number);

			// `submit_unsigned_transaction` returns a type of `Result<(), ()>`
			//   ref: https://substrate.dev/rustdocs/v2.0.0/frame_system/offchain/struct.SubmitTransaction.html#method.submit_unsigned_transaction
			SubmitTransaction::<T, Call<T>>::submit_unsigned_transaction(call.into())
			.map_err(|_| {
				log::error!("Failed in offchain_unsigned_tx");
				<Error<T>>::OffchainUnsignedTxError
			})
		}

		fn offchain_unsigned_tx_signed_payload(block_number: T::BlockNumber) -> Result<(), Error<T>> {
			// Retrieve the signer to sign the payload
			let signer = Signer::<T, T::AuthorityId>::any_account();

			let number: u64 = block_number.try_into().unwrap_or(0);

			// `send_unsigned_transaction` is returning a type of `Option<(Account<T>, Result<(), ()>)>`.
			//   Similar to `send_signed_transaction`, they account for:
			//   - `None`: no account is available for sending transaction
			//   - `Some((account, Ok(())))`: transaction is successfully sent
			//   - `Some((account, Err(())))`: error occured when sending the transaction
			if let Some((_, res)) = signer.send_unsigned_transaction(
				|acct| Payload { number, public: acct.public.clone() },
				Call::submit_number_unsigned_with_signed_payload
				) {
				return res.map_err(|_| {
					log::error!("Failed in offchain_unsigned_tx_signed_payload");
					<Error<T>>::OffchainUnsignedTxSignedPayloadError
				});
			}

			// The case of `None`: no account is available for sending
			log::error!("No local account available");
			Err(<Error<T>>::NoLocalAcctForSigning)
		}
	}

	impl<T: Config> BlockNumberProvider for Pallet<T> {
		    type BlockNumber = T::BlockNumber;
			
			fn current_block_number() -> Self::BlockNumber {
				<frame_system::Pallet<T>>::block_number()
		}
	}
}