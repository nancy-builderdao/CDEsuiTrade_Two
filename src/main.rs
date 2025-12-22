use std::error::Error;

use bech32::FromBase32;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_crypto::SuiSigner;
use sui_rpc::Client;
use sui_rpc::proto::sui::rpc::v2::{ListOwnedObjectsRequest, GetObjectRequest};
use sui_sdk_types::{Address, Digest};
use sui_transaction_builder::unresolved::Input;
use prost_types::FieldMask;
use sui_rpc::proto::sui::rpc::v2::Object;
use tokio::time::Instant;

mod momentum;
mod cetus;
mod bluefin;

/// Enable / disable debug logs in main.rs.
const DEBUG_MAIN: bool = true;

/// Default swap amount (in smallest unit of the token).
const DEFAULT_SWAP_AMOUNT: u64 = 1000;

/// Default gas budget and gas price.
const DEFAULT_GAS_BUDGET: u64 = 500_000_00;
const DEFAULT_GAS_PRICE: u64 = 1_000;

/// Hard-coded pool object id and token object id used in the Momentum example.
const DEFAULT_POOL_ID: &str =
    "0x455cf8d2ac91e7cb883f515874af750ed3cd18195c970b7a2d46235ac2b0c388";
const DEFAULT_TOKEN_OBJECT_ID: &str =
    "0x73f4a6e1b0cc88644de7017419a47e85ebbf1d4230f71d4c3c30cd2cd87bafaf";

/// Example private key (bech32 suiprivkey format).
const EXAMPLE_PRIVATE_KEY: &str = "input_yout_private_key";

const VERSIONED_OBJECT_ID: &str =
    "0x2375a0b1ec12010aaea3b2545acfa2ad34cfbba03ce4b59f4c39e1e25eed1b2a";

// ====== Cetus specific constants ======
const CETUS_GLOBAL_CONFIG_ID: &str =
    "0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f";
const CETUS_POOL_ID: &str =
    "0x51e883ba7c0b566a26cbc8a94cd33eb0abd418a77cc1e60ad22fd9b1f29cd2ab"; // Replace with actual pool id
const CETUS_TOKEN_OBJECT_ID: &str =
    "0x73f4a6e1b0cc88644de7017419a47e85ebbf1d4230f71d4c3c30cd2cd87bafaf"; // Replace with actual token object id

// Token types for Cetus
const CETUS_TOKEN_A_TYPE: &str = "0x2::sui::SUI";
const CETUS_TOKEN_B_TYPE: &str =
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

// ====== Bluefin specific constants ======
const BLUEFIN_GLOBAL_CONFIG_ID: &str =
    "0x03db251ba509a8d5d8777b6338836082335d93eecbdd09a11e190a1cff51c352";
const BLUEFIN_POOL_ID: &str =
    "0x15dbcac854b1fc68fc9467dbd9ab34270447aabd8cc0e04a5864d95ccb86b74a";
const BLUEFIN_TOKEN_OBJECT_ID: &str =
    "0x73f4a6e1b0cc88644de7017419a47e85ebbf1d4230f71d4c3c30cd2cd87bafaf"; // Replace with actual token object id

// Token types for Bluefin
const BLUEFIN_TOKEN_A_TYPE: &str = "0x2::sui::SUI";
const BLUEFIN_TOKEN_B_TYPE: &str =
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

/// Swap type selection.
#[derive(Debug, Clone, Copy)]
enum SwapType {
    Momentum,
    Cetus,
    Bluefin,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Select which swap to execute.
    let swap_type = SwapType::Cetus; // Change to SwapType::Cetus or SwapType::Bluefin

    match swap_type {
        SwapType::Momentum => run_momentum_swap().await,
        SwapType::Cetus => run_cetus_swap().await,
        SwapType::Bluefin => run_bluefin_swap().await,
    }
}

/// Run Momentum swap transaction.
async fn run_momentum_swap() -> Result<(), Box<dyn Error>> {
    debug_main("[run_momentum_swap] start");
    let start = Instant::now();

    // 1. Decode private key from bech32 "suiprivkey..." format.
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();

    println!("Owner address: {:?}", owner_address);
    debug_main("[run_momentum_swap] decoded private key and derived address");

    // 2. Create Sui gRPC client.
    let mut client = Client::new("http://3.114.103.176:443")?;
    println!("Sui gRPC client connected");
    debug_main("[run_momentum_swap] Sui gRPC client created");

    // 3. Query owned SUI coins to get a gas object id.
    let gas_object_id = fetch_first_sui_gas_object_id(&mut client, &owner_address).await?;
    println!("Selected gas object id: {:?}", gas_object_id);
    debug_main(&format!(
        "[run_momentum_swap] fetched gas object id: {gas_object_id}"
    ));

    // 4. Prepare swap parameters.
    let pool_object_id: Address = DEFAULT_POOL_ID.parse()?;
    let token_object_id: Address = DEFAULT_TOKEN_OBJECT_ID.parse()?;
    let versioned_object_id: Address = VERSIONED_OBJECT_ID.parse()?;
    let clock_object_id: Address = "0x6".parse()?;

    // Fetch object details.
    let gas_obj = fetch_object_details(&mut client, gas_object_id).await?;
    let pool_obj = fetch_object_details(&mut client, pool_object_id).await?;
    let token_obj = fetch_object_details(&mut client, token_object_id).await?;
    let version_obj = fetch_object_details(&mut client, versioned_object_id).await?;
    let clock_obj = fetch_object_details(&mut client, clock_object_id).await?;

    // Construct Inputs.
    // Token (Owned)
    let token_version = token_obj.version.ok_or("Missing version for token")?;
    let token_digest_str = token_obj.digest.ok_or("Missing digest for token")?;
    let token_digest: Digest = token_digest_str.parse()?;

    let gas_version = gas_obj.version.ok_or("Missing version for gas object")?;
    let gas_digest_str = gas_obj.digest.ok_or("Missing digest for gas object")?;
    let gas_digest: Digest = gas_digest_str.parse()?;

    let gas_input = Input::by_id(gas_object_id)
        .with_owned_kind()
        .with_version(gas_version)
        .with_digest(gas_digest);

    let token_input = Input::by_id(token_object_id)
        .with_owned_kind()
        .with_version(token_version)
        .with_digest(token_digest);

    // Pool (Shared)
    let initial_shared_version = get_initial_shared_version(&pool_obj)?;
    let clock_version = get_initial_shared_version(&clock_obj)?;
    let version_version = get_initial_shared_version(&version_obj)?;
    println!(
        "Initial shared versions - pool: {}, clock: {}, versioned: {}",
        initial_shared_version, clock_version, version_version
    );

    let pool_input = Input::by_id(pool_object_id)
        .with_shared_kind()
        .with_initial_shared_version(initial_shared_version)
        .by_val();

    let clock_input = Input::by_id(clock_object_id)
        .with_shared_kind()
        .with_initial_shared_version(clock_version)
        .by_ref();

    let version_input = Input::by_id(versioned_object_id)
        .with_shared_kind()
        .with_initial_shared_version(version_version)
        .by_val();

    let amount: u64 = DEFAULT_SWAP_AMOUNT;
    let direction: bool = false; // true: A -> B, false: B -> A

    debug_main(&format!(
        "[run_momentum_swap] swap params: token={token_object_id}, pool={pool_object_id}, amount={amount}, direction={direction}"
    ));

    // 5. Build transaction.
    debug_main("[run_momentum_swap] before create_swap_transaction");
    let tx = momentum::create_swap_transaction(
        token_input,
        pool_input,
        gas_input,
        amount,
        direction,
        owner_address,
        DEFAULT_GAS_BUDGET,
        DEFAULT_GAS_PRICE,
        clock_input,
        version_input,
    )?;
    debug_main("[run_momentum_swap] after create_swap_transaction (tx built)");

    // 6. Sign transaction.
    let signature = private_key.sign_transaction(&tx)?;
    debug_main("[run_momentum_swap] transaction signed");

    // 7. Execute transaction.
    let mut exec_client = client.execution_client();

    let mut request = sui_rpc::proto::sui::rpc::v2::ExecuteTransactionRequest::default();
    request.transaction = Some(tx.into());
    request.signatures = vec![signature.into()];

    debug_main("[run_momentum_swap] before execute_transaction");
    let response = exec_client.execute_transaction(request).await?;
    debug_main("[run_momentum_swap] after execute_transaction");

    let elapsed = start.elapsed();
    println!(
        "Transaction submitted, response: {:?}",
        response.into_inner()
    );
    println!("Elapsed time: {:.3?}", elapsed);

    Ok(())
}

/// Run Cetus swap transaction.
async fn run_cetus_swap() -> Result<(), Box<dyn Error>> {
    debug_main("[run_cetus_swap] start");
    let start = Instant::now();

    // 1. Decode private key from bech32 "suiprivkey..." format.
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();

    println!("Owner address: {:?}", owner_address);
    debug_main("[run_cetus_swap] decoded private key and derived address");

    // 2. Create Sui gRPC client.
    let mut client = Client::new("http://3.114.103.176:443")?;
    println!("Sui gRPC client connected");
    debug_main("[run_cetus_swap] Sui gRPC client created");

    // 3. Query owned SUI coins to get a gas object id.
    let gas_object_id = fetch_first_sui_gas_object_id(&mut client, &owner_address).await?;
    println!("Selected gas object id: {:?}", gas_object_id);
    debug_main(&format!(
        "[run_cetus_swap] fetched gas object id: {gas_object_id}"
    ));

    // 4. Prepare swap parameters.
    let pool_object_id: Address = CETUS_POOL_ID.parse()?;
    let token_object_id: Address = CETUS_TOKEN_OBJECT_ID.parse()?;
    let global_config_id: Address = CETUS_GLOBAL_CONFIG_ID.parse()?;
    let clock_object_id: Address = "0x6".parse()?;

    // Fetch object details.
    let gas_obj = fetch_object_details(&mut client, gas_object_id).await?;
    let pool_obj = fetch_object_details(&mut client, pool_object_id).await?;
    let token_obj = fetch_object_details(&mut client, token_object_id).await?;
    let global_config_obj = fetch_object_details(&mut client, global_config_id).await?;
    let clock_obj = fetch_object_details(&mut client, clock_object_id).await?;

    // Construct Inputs.
    // Gas (Owned)
    let gas_version = gas_obj.version.ok_or("Missing version for gas object")?;
    let gas_digest_str = gas_obj.digest.ok_or("Missing digest for gas object")?;
    let gas_digest: Digest = gas_digest_str.parse()?;

    let gas_input = Input::by_id(gas_object_id)
        .with_owned_kind()
        .with_version(gas_version)
        .with_digest(gas_digest);

    // Token (Owned)
    let token_version = token_obj.version.ok_or("Missing version for token")?;
    let token_digest_str = token_obj.digest.ok_or("Missing digest for token")?;
    let token_digest: Digest = token_digest_str.parse()?;

    let token_input = Input::by_id(token_object_id)
        .with_owned_kind()
        .with_version(token_version)
        .with_digest(token_digest);

    // Pool (Shared)
    let pool_shared_version = get_initial_shared_version(&pool_obj)?;
    let pool_input = Input::by_id(pool_object_id)
        .with_shared_kind()
        .with_initial_shared_version(pool_shared_version)
        .by_val();

    // Global Config (Shared)
    let global_config_shared_version = get_initial_shared_version(&global_config_obj)?;
    let global_config_input = Input::by_id(global_config_id)
        .with_shared_kind()
        .with_initial_shared_version(global_config_shared_version)
        .by_ref();

    // Clock (Shared)
    let clock_shared_version = get_initial_shared_version(&clock_obj)?;
    let clock_input = Input::by_id(clock_object_id)
        .with_shared_kind()
        .with_initial_shared_version(clock_shared_version)
        .by_ref();

    println!(
        "Initial shared versions - pool: {}, global_config: {}, clock: {}",
        pool_shared_version, global_config_shared_version, clock_shared_version
    );

    let amount_in: u64 = DEFAULT_SWAP_AMOUNT;
    let min_amount_out: u64 = 0; // Set appropriate slippage protection
    let a2b: bool = false; // true: A -> B, false: B -> A

    debug_main(&format!(
        "[run_cetus_swap] swap params: token={token_object_id}, pool={pool_object_id}, amount_in={amount_in}, min_amount_out={min_amount_out}, a2b={a2b}"
    ));

    // 5. Build transaction.
    debug_main("[run_cetus_swap] before create_cetus_swap_transaction");
    let tx = cetus::create_cetus_swap_transaction(
        token_input,
        pool_input,
        global_config_input,
        clock_input,
        gas_input,
        amount_in,
        min_amount_out,
        a2b,
        owner_address,
        DEFAULT_GAS_BUDGET,
        DEFAULT_GAS_PRICE,
        CETUS_TOKEN_A_TYPE,
        CETUS_TOKEN_B_TYPE,
    )?;
    debug_main("[run_cetus_swap] after create_cetus_swap_transaction (tx built)");

    // 6. Sign transaction.
    let signature = private_key.sign_transaction(&tx)?;
    debug_main("[run_cetus_swap] transaction signed");

    // 7. Execute transaction.
    let mut exec_client = client.execution_client();

    let mut request = sui_rpc::proto::sui::rpc::v2::ExecuteTransactionRequest::default();
    request.transaction = Some(tx.into());
    request.signatures = vec![signature.into()];

    debug_main("[run_cetus_swap] before execute_transaction");
    let response = exec_client.execute_transaction(request).await?;
    debug_main("[run_cetus_swap] after execute_transaction");

    let elapsed = start.elapsed();
    println!(
        "Transaction submitted, response: {:?}",
        response.into_inner()
    );
    println!("Elapsed time: {:.3?}", elapsed);

    Ok(())
}

/// Run Bluefin swap transaction.
async fn run_bluefin_swap() -> Result<(), Box<dyn Error>> {
    debug_main("[run_bluefin_swap] start");
    let start = Instant::now();

    // 1. Decode private key from bech32 "suiprivkey..." format.
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();

    println!("Owner address: {:?}", owner_address);
    debug_main("[run_bluefin_swap] decoded private key and derived address");

    // 2. Create Sui gRPC client.
    let mut client = Client::new("http://3.114.103.176:443")?;
    println!("Sui gRPC client connected");
    debug_main("[run_bluefin_swap] Sui gRPC client created");

    // 3. Query owned SUI coins to get a gas object id.
    let gas_object_id = fetch_first_sui_gas_object_id(&mut client, &owner_address).await?;
    println!("Selected gas object id: {:?}", gas_object_id);
    debug_main(&format!(
        "[run_bluefin_swap] fetched gas object id: {gas_object_id}"
    ));

    // 4. Prepare swap parameters.
    let pool_object_id: Address = BLUEFIN_POOL_ID.parse()?;
    let token_object_id: Address = BLUEFIN_TOKEN_OBJECT_ID.parse()?;
    let global_config_id: Address = BLUEFIN_GLOBAL_CONFIG_ID.parse()?;
    let clock_object_id: Address = "0x6".parse()?;

    // Fetch object details.
    let gas_obj = fetch_object_details(&mut client, gas_object_id).await?;
    let pool_obj = fetch_object_details(&mut client, pool_object_id).await?;
    let token_obj = fetch_object_details(&mut client, token_object_id).await?;
    let global_config_obj = fetch_object_details(&mut client, global_config_id).await?;
    let clock_obj = fetch_object_details(&mut client, clock_object_id).await?;

    // Construct Inputs.
    // Gas (Owned)
    let gas_version = gas_obj.version.ok_or("Missing version for gas object")?;
    let gas_digest_str = gas_obj.digest.ok_or("Missing digest for gas object")?;
    let gas_digest: Digest = gas_digest_str.parse()?;

    let gas_input = Input::by_id(gas_object_id)
        .with_owned_kind()
        .with_version(gas_version)
        .with_digest(gas_digest);

    // Token (Owned)
    let token_version = token_obj.version.ok_or("Missing version for token")?;
    let token_digest_str = token_obj.digest.ok_or("Missing digest for token")?;
    let token_digest: Digest = token_digest_str.parse()?;

    let token_input = Input::by_id(token_object_id)
        .with_owned_kind()
        .with_version(token_version)
        .with_digest(token_digest);

    // Pool (Shared)
    let pool_shared_version = get_initial_shared_version(&pool_obj)?;
    let pool_input = Input::by_id(pool_object_id)
        .with_shared_kind()
        .with_initial_shared_version(pool_shared_version)
        .by_val();

    // Global Config (Shared)
    let global_config_shared_version = get_initial_shared_version(&global_config_obj)?;
    let global_config_input = Input::by_id(global_config_id)
        .with_shared_kind()
        .with_initial_shared_version(global_config_shared_version)
        .by_ref();

    // Clock (Shared)
    let clock_shared_version = get_initial_shared_version(&clock_obj)?;
    let clock_input = Input::by_id(clock_object_id)
        .with_shared_kind()
        .with_initial_shared_version(clock_shared_version)
        .by_ref();

    println!(
        "Initial shared versions - pool: {}, global_config: {}, clock: {}",
        pool_shared_version, global_config_shared_version, clock_shared_version
    );

    let amount_in: u64 = DEFAULT_SWAP_AMOUNT;
    let a2b: bool = false; // true: SUI -> USDC, false: USDC -> SUI

    debug_main(&format!(
        "[run_bluefin_swap] swap params: token={token_object_id}, pool={pool_object_id}, amount_in={amount_in}, a2b={a2b}"
    ));

    // 5. Build transaction.
    debug_main("[run_bluefin_swap] before create_bluefin_swap_transaction");
    let tx = bluefin::create_bluefin_swap_transaction(
        token_input,
        pool_input,
        global_config_input,
        clock_input,
        gas_input,
        amount_in,
        a2b,
        owner_address,
        DEFAULT_GAS_BUDGET,
        DEFAULT_GAS_PRICE,
        BLUEFIN_TOKEN_A_TYPE,
        BLUEFIN_TOKEN_B_TYPE,
    )?;
    debug_main("[run_bluefin_swap] after create_bluefin_swap_transaction (tx built)");

    // 6. Sign transaction.
    let signature = private_key.sign_transaction(&tx)?;
    debug_main("[run_bluefin_swap] transaction signed");

    // 7. Execute transaction.
    let mut exec_client = client.execution_client();

    let mut request = sui_rpc::proto::sui::rpc::v2::ExecuteTransactionRequest::default();
    request.transaction = Some(tx.into());
    request.signatures = vec![signature.into()];

    debug_main("[run_bluefin_swap] before execute_transaction");
    let response = exec_client.execute_transaction(request).await?;
    debug_main("[run_bluefin_swap] after execute_transaction");

    let elapsed = start.elapsed();
    println!(
        "Transaction submitted, response: {:?}",
        response.into_inner()
    );
    println!("Elapsed time: {:.3?}", elapsed);

    Ok(())
}

/// Decode Sui Ed25519 private key from bech32 "suiprivkey..." string.
fn decode_sui_private_key(key_str: &str) -> Result<Ed25519PrivateKey, Box<dyn Error>> {
    let (_hrp, data, _variant) = bech32::decode(key_str)?;
    let bytes = Vec::<u8>::from_base32(&data)?;

    if bytes.len() != 33 || bytes[0] != 0 {
        return Err("Invalid Sui private key format".into());
    }

    let pk_bytes: [u8; 32] = bytes[1..]
        .try_into()
        .map_err(|_| "Invalid Sui private key length")?;

    Ok(Ed25519PrivateKey::new(pk_bytes))
}

/// Fetch the first owned SUI coin object id for the given address.
async fn fetch_first_sui_gas_object_id(
    client: &mut Client,
    owner: &Address,
) -> Result<Address, Box<dyn Error>> {
    let mut state_client = client.state_client();

    let mut request = ListOwnedObjectsRequest::default();
    request.owner = Some(owner.to_string());
    request.page_size = Some(1000);
    request.object_type = Some("0x2::coin::Coin<0x2::sui::SUI>".to_string());

    let mut mask = prost_types::FieldMask::default();
    mask.paths = vec!["object_id".to_string()];
    request.read_mask = Some(mask);

    let response = state_client.list_owned_objects(request).await?.into_inner();
    println!("Owned SUI objects response: {:?}", response);

    if response.objects.is_empty() {
        return Err("No SUI gas objects found for this address".into());
    }

    // Use the first SUI coin object as gas.
    let obj = &response.objects[0];

    let oid_str = obj
        .object_id
        .as_ref()
        .ok_or("Missing object_id field in ListOwnedObjectsResponse")?;

    let oid: Address = oid_str.parse()?;
    Ok(oid)
}

fn debug_main(msg: &str) {
    if DEBUG_MAIN {
        eprintln!("{msg}");
    }
}

async fn fetch_object_details(
    client: &mut Client,
    object_id: Address,
) -> Result<Object, Box<dyn std::error::Error>> {
    let mut ledger_client = client.ledger_client();

    let mut request = GetObjectRequest::new(&object_id);

    request.read_mask = Some(FieldMask {
        paths: vec![
            "object_id".to_string(),
            "version".to_string(),
            "digest".to_string(),
            "owner".to_string(),
        ],
    });

    let response = ledger_client.get_object(request).await?.into_inner();
    response.object.ok_or_else(|| "Object not found".into())
}

fn get_initial_shared_version(
    obj: &sui_rpc::proto::sui::rpc::v2::Object,
) -> Result<u64, Box<dyn Error>> {
    println!("Object details: {:?}", obj);
    if let Some(ref owner) = obj.owner {
        return Ok(owner.version());
    }
    Err("Object is not shared or missing owner field".into())
}