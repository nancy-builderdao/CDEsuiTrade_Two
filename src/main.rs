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
use sui_rpc::proto::sui::rpc::v2::SubscribeCheckpointsRequest;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;


mod momentum;
mod cetus;
mod bluefin;

/// Enable / disable debug logs in main.rs.
const DEBUG_MAIN: bool = true;

/// Default swap amount (in smallest unit of the token).
const DEFAULT_SWAP_AMOUNT: u64 = 1000000;

/// Default gas budget and gas price.
const DEFAULT_GAS_BUDGET: u64 = 500_000_00;
const DEFAULT_GAS_PRICE: u64 = 1_000;

/// Hard-coded pool object id and token object id used in the Momentum example.
const DEFAULT_POOL_ID: &str =
    "0x455cf8d2ac91e7cb883f515874af750ed3cd18195c970b7a2d46235ac2b0c388";
const DEFAULT_TOKEN_OBJECT_ID: &str =
    "0x66bcedb93c0a58689944a5b8fb532e80c61300c8f8bf608f47d35dd0736c91b5";

/// Example private key (bech32 suiprivkey format).
const EXAMPLE_PRIVATE_KEY: &str = "suiprivkey1qzcq4jx6g0a8jmpwer0wfpr5kc8r2mfrmklj2a7f72xft2ff36w2wmsvyf4";

const VERSIONED_OBJECT_ID: &str =
    "0x2375a0b1ec12010aaea3b2545acfa2ad34cfbba03ce4b59f4c39e1e25eed1b2a";

// ====== Cetus specific constants ======
const CETUS_GLOBAL_CONFIG_ID: &str =
    "0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f";
const CETUS_POOL_ID: &str =
    "0x51e883ba7c0b566a26cbc8a94cd33eb0abd418a77cc1e60ad22fd9b1f29cd2ab"; // Replace with actual pool id
const CETUS_TOKEN_OBJECT_ID: &str =
    "0x66bcedb93c0a58689944a5b8fb532e80c61300c8f8bf608f47d35dd0736c91b5"; // Replace with actual token object id

// Token types for Cetus
const CETUS_TOKEN_A_TYPE: &str = "0x2::sui::SUI";
const CETUS_TOKEN_B_TYPE: &str =
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

// ====== Bluefin specific constants ======
const BLUEFIN_GLOBAL_CONFIG_ID: &str =
    "0x03db251ba509a8d5d8777b6338836082335d93eecbdd09a11e190a1cff51c352";
const BLUEFIN_POOL_ID: &str =
    "0x15dbcac854b1fc68fc9467dbd9ab34270447aabd8cc0e04a5864d95ccb86b74a";
//const BLUEFIN_TOKEN_OBJECT_ID: &str =
//    "0x66bcedb93c0a58689944a5b8fb532e80c61300c8f8bf608f47d35dd0736c91b5"; // Replace with actual token object id

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

#[derive(Debug)]
struct TradeStats {
    latency_ms: u128,
    lag: i64,
    price_diff: f64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 建立連線 (這部分可以共用，不用在迴圈內重建 connection，但 subscription 要重建)
    let mut monitor_client = Client::new("http://3.114.103.176:443")?;
    let mut action_client = Client::new("http://3.114.103.176:443")?; 
    
    // 建立通道
    let (tx, mut rx) = mpsc::channel::<TradeStats>(10);
    
    let mut results: Vec<TradeStats> = Vec::new();
    let target_runs = 100;

    println!("🚀 gRPC 監控啟動 (每次重連模式)，準備執行 {} 次測試...", target_runs);

    // ✨ 修改點：改用 for 迴圈主導流程，而不是 stream.next()
    for round in 1..=target_runs {
        println!("\n================ [第 {} / {} 次測試] ================", round, target_runs);
        println!("📡 正在訂閱最新的 Checkpoint...");

        // 1. 每次迴圈重新建立 Subscription Client 與 Stream
        // 這樣可以確保不會讀到「上一輪執行期間」堆積在緩衝區的舊資料
        let mut sub_client = monitor_client.subscription_client();
        let stream_result = sub_client.subscribe_checkpoints(SubscribeCheckpointsRequest::default()).await;

        match stream_result {
            Ok(stream_response) => {
                let mut stream = stream_response.into_inner();

                // 2. 只等待「下一筆」資料 (這必定是建立連線後的最新一筆)
                if let Some(item) = stream.next().await {
                    match item {
                        Ok(resp) => {
                            let cursor = resp.cursor.unwrap_or_default();
                            println!("⚡️ 收到最新 Checkpoint: {}", cursor);

                            let mut trigger_price = 0.0;
                            match fetch_current_pool_price(&mut monitor_client, BLUEFIN_POOL_ID).await {
                                Ok(price) => {
                                    trigger_price = price;
                                    println!("💰 當前 gRPC 池子價格: {:.4}", price);
                                },
                                Err(e) => eprintln!("⚠️ 查價失敗: {}", e),
                            }

                            // ✨ 修改：把 trigger_price 傳進去
                            if let Err(e) = run_bluefin_swap(&mut action_client, cursor, tx.clone(), trigger_price).await {
                                eprintln!("❌ 交易執行失敗: {}", e);
                            }
                            
                            // 4. 等待分析結果 (這時候 Stream 會被擱置，但我們不在乎了，因為下一輪會開新的)
                            println!("⏳ 等待分析結果...");
                            if let Some(stats) = rx.recv().await {
                                println!("📝 記錄數據: Latency={}ms, Lag={}", stats.latency_ms, stats.lag);
                                results.push(stats);
                            }
                        }
                        Err(e) => eprintln!("Stream error: {}", e),
                    }
                }
                // 離開 if let，stream 會被 Drop 掉，斷開訂閱
            }
            Err(e) => eprintln!("訂閱失敗: {}", e),
        }

        // (選用) 稍微冷卻一下，確保跟上一輪徹底切開，避免連線頻率限制
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    println!("\n========================================");
    println!("📊 {} 次執行總結報告", target_runs);
    println!("========================================");

    if !results.is_empty() {
        let count = results.len() as f64;
        
        let avg_latency = results.iter().map(|s| s.latency_ms as f64).sum::<f64>() / count;
        let avg_lag = results.iter().map(|s| s.lag as f64).sum::<f64>() / count;
        // 取絕對值 (abs) 來計算誤差幅度，避免正負抵銷
        let avg_diff = results.iter().map(|s| s.price_diff.abs()).sum::<f64>() / count;

        println!("✅ 成功樣本數: {} / {}", results.len(), target_runs);
        println!("⏱️ 平均區塊延遲: {:.2} blocks", avg_lag);
        println!("💵 平均價格誤差: {:.4}%", avg_diff);
        println!("⚡️ 平均執行耗時: {:.2} ms", avg_latency);

        println!("\n📋 [詳細數據列表] (Round | Latency | Lag | Diff%)");
        println!("--------------------------------------------------");
        for (i, stats) in results.iter().enumerate() {
            println!(
                "{:<3} | {:<4} ms | {:<2} blocks | {:.4}%", 
                i + 1, 
                stats.latency_ms, 
                stats.lag, 
                stats.price_diff
            );
        }
        println!("--------------------------------------------------\n");
    } else {
        println!("❌ 沒有成功收集到數據");
    }

    Ok(())
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
async fn run_bluefin_swap(
    client: &mut Client, 
    trigger_checkpoint: u64,
    tx_sender: mpsc::Sender<TradeStats>,
    trigger_price: f64
) -> Result<(), Box<dyn Error>> {
    debug_main("[run_bluefin_swap] start");
    let start = Instant::now();

    // 1. Decode private key from bech32 "suiprivkey..." format.
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();

    println!("Owner address: {:?}", owner_address);
    debug_main("[run_bluefin_swap] decoded private key and derived address");

    // 2. Create Sui gRPC client.
    //let mut client = Client::new("http://3.114.103.176:443")?;
    println!("Sui gRPC client connected");
    debug_main("[run_bluefin_swap] Sui gRPC client created");

    // 3. Query owned SUI coins to get a gas object id.
    let gas_object_id = fetch_first_sui_gas_object_id(client, &owner_address).await?;
    println!("Selected gas object id: {:?}", gas_object_id);
    debug_main(&format!(
        "[run_bluefin_swap] fetched gas object id: {gas_object_id}"
    ));

    // 4. Prepare swap parameters.
    let pool_object_id: Address = BLUEFIN_POOL_ID.parse()?;
    //let token_object_id: Address = BLUEFIN_TOKEN_OBJECT_ID.parse()?;
    let token_object_id = fetch_sui_coin_excluding_gas(client, &owner_address, gas_object_id).await?;
    let global_config_id: Address = BLUEFIN_GLOBAL_CONFIG_ID.parse()?;
    let clock_object_id: Address = "0x6".parse()?;

    // Fetch object details.
    let gas_obj = fetch_object_details(client, gas_object_id).await?;
    let pool_obj = fetch_object_details(client, pool_object_id).await?;
    let token_obj = fetch_object_details(client, token_object_id).await?;
    let global_config_obj = fetch_object_details(client, global_config_id).await?;
    let clock_obj = fetch_object_details(client, clock_object_id).await?;

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
    let a2b: bool = true; // true: SUI -> USDC, false: USDC -> SUI

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
    let resp_inner = response.into_inner();

    //println!(
    //    "Transaction submitted, response: {:?}",
    //    resp_inner
    //);
    println!("Elapsed time: {:.3?}", elapsed);

    // 抓取交易 Digest
    let tx_digest = resp_inner.transaction.as_ref()
        .and_then(|t| t.effects.as_ref()) 
        .and_then(|e| e.transaction_digest.as_ref()) 
        .map(|d| d.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    println!(
        "🔔 Trigger Checkpoint: {} | ✅ Tx Digest: {} | ⏱️ Latency: {:.3?}",
        trigger_checkpoint, tx_digest, elapsed
    );

    if tx_digest != "Unknown" {
        let digest_clone = tx_digest.clone();
        let latency_ms = elapsed.as_millis(); // 轉成 ms
        
        tokio::spawn(async move {
            check_lag_background(tx_sender, digest_clone, trigger_checkpoint, latency_ms, trigger_price).await;
        });
    } else {
        // 如果交易失敗沒 Digest，也送一個空的結果回去，避免主程式卡死
        let _ = tx_sender.send(TradeStats { 
            latency_ms: elapsed.as_millis(), 
            lag: -1,
            price_diff: 0.0 // ✨ 修正：補上這個欄位，預設為 0.0
        }).await;
    }

    Ok(())
}

// ✨ 新增：計算 Bluefin 價格 (把 u128 的 sqrt_price 轉成人類看得懂的價格)
fn calculate_bluefin_price(sqrt_price_str: &str, is_sui_usdc: bool) -> Result<f64, Box<dyn Error>> {
    let sqrt_price = sqrt_price_str.parse::<u128>()?;
    
    // Q64.64 定點數轉換
    let q64 = (1u128 << 64) as f64;
    let price_raw = (sqrt_price as f64 / q64).powi(2);

    // 如果是 SUI/USDC (SUI=9 decimals, USDC=6 decimals)
    // 價格通常是 USDC / SUI，所以要乘上 10^(9-6) = 1000
    // 如果你的觀察發現價格差 1000 倍，請調整這個 multiplier
    let multiplier = if is_sui_usdc { 1000.0 } else { 1.0 };
    
    Ok(price_raw * multiplier)
}

async fn fetch_current_pool_price(client: &mut Client, pool_id_str: &str) -> Result<f64, Box<dyn Error>> {
    let mut ledger_client = client.ledger_client();
    let pool_id: Address = pool_id_str.parse()?;
    
    let mut request = GetObjectRequest::new(&pool_id);
    request.read_mask = Some(FieldMask {
        paths: vec!["json".to_string()] 
    });

    let response = ledger_client.get_object(request).await?.into_inner();
    
    if let Some(json_content) = response.object.and_then(|o| o.json) {
        if let Some(prost_types::value::Kind::StructValue(st)) = &json_content.kind {
            // 使用新的遞迴搜尋
            if let Some(price_str) = extract_price_from_prost_struct(st) {
                 return calculate_bluefin_price(&price_str, true);
            } else {
                // ✨ Debug: 如果找不到，印出頂層有哪些 key，方便除錯
                let keys: Vec<&String> = st.fields.keys().collect();
                eprintln!("⚠️ gRPC JSON 結構中找不到 current_sqrt_price。頂層 Keys: {:?}", keys);
            }
        }
    }
    
    Err("無法解析 Pool 價格 (欄位結構不符)".into())
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

// ✨ 新增：找出一個不是 Gas 的 SUI Coin
async fn fetch_sui_coin_excluding_gas(
    client: &mut Client,
    owner: &Address,
    gas_id: Address,
) -> Result<Address, Box<dyn Error>> {
    let mut state_client = client.state_client();

    let mut request = ListOwnedObjectsRequest::default();
    request.owner = Some(owner.to_string());
    request.page_size = Some(1000); // 抓多一點確保能找到第二個
    // 這裡假設我們要 Swap 的是 SUI，如果我們要 Swap 其他幣種 (如 USDC)，要改這裡的 Type
    request.object_type = Some("0x2::coin::Coin<0x2::sui::SUI>".to_string());
    request.read_mask = Some(FieldMask { paths: vec!["object_id".to_string()] });

    // 取得列表
    let response = state_client.list_owned_objects(request).await?.into_inner();
    
    // 遍歷所有 SUI Coin，找出第一個 ID 不等於 gas_id 的
    for obj in response.objects {
        if let Some(oid_str) = obj.object_id.as_ref() {
            let oid: Address = oid_str.parse()?;
            if oid != gas_id {
                return Ok(oid);
            }
        }
    }
    
    Err("無法找到第二個 SUI Coin (你需要至少有兩個 SUI Objects，一個付 Gas，一個做交易)".into())
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

async fn check_lag_background(
    tx_sender: mpsc::Sender<TradeStats>, 
    tx_digest: String, 
    trigger_checkpoint: u64,
    latency_ms: u128,      // ✨ 修正：補上逗號
    trigger_price: f64     // ✨ 這是我們從 gRPC 查到的價格
) {
    // 1. 等待節點索引
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let rpc_url = "http://3.114.103.176:443";
    let client = reqwest::Client::new();

    // ✨ 修改：必須把 showEffects 和 showBalanceChanges 設為 true 才能算價格
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sui_getTransactionBlock",
        "params": [
            tx_digest,
            {
                "showInput": false,
                "showRawInput": false,
                "showEffects": true,          // ✨ 改為 true (為了算 Gas)
                "showEvents": false,
                "showObjectChanges": false,
                "showBalanceChanges": true    // ✨ 改為 true (為了算 Swap 金額)
            }
        ]
    });

    let mut lag_result: i64 = 0; 
    let mut price_diff_result: f64 = 0.0;

    match client.post(rpc_url).json(&body).send().await {
        Ok(resp) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(tx_cp_str) = json["result"]["checkpoint"].as_str() {
                    if let Ok(tx_cp) = tx_cp_str.parse::<u64>() {
                        let lag = tx_cp as i64 - trigger_checkpoint as i64;
                        
                        println!("\n📊 [交易分析] Tx: {}", tx_digest);
                        println!("   -----------------------------------------");
                        println!(
                            "   ⏱️ Checkpoint Lag: {} (Trigger: {} -> On-Chain: {})", 
                            lag, trigger_checkpoint, tx_cp
                        );

                        // === ✨✨✨ 移植的價格計算邏輯開始 ✨✨✨ ===
                        
                        // 1. 計算 Net Gas Fee
                        let mut net_gas_fee: u64 = 0;
                        if let Some(gas_summary) = json["result"]["effects"]["gasUsed"].as_object() {
                            let comp = gas_summary.get("computationCost").and_then(|v| v.as_str()).unwrap_or("0").parse::<u64>().unwrap_or(0);
                            let storage = gas_summary.get("storageCost").and_then(|v| v.as_str()).unwrap_or("0").parse::<u64>().unwrap_or(0);
                            let rebate = gas_summary.get("storageRebate").and_then(|v| v.as_str()).unwrap_or("0").parse::<u64>().unwrap_or(0);
                            
                            let total_cost = comp + storage;
                            if total_cost > rebate {
                                net_gas_fee = total_cost - rebate;
                            }
                        }

                        // 2. 解析 Balance Changes
                        let mut swap_sui_in = 0.0;
                        let mut swap_usdc_out = 0.0;

                        if let Some(changes) = json["result"]["balanceChanges"].as_array() {
                            for change in changes {
                                let coin_type = change["coinType"].as_str().unwrap_or("");
                                let amount_str = change["amount"].as_str().unwrap_or("0");
                                
                                if let Ok(amount_i128) = amount_str.parse::<i128>() {
                                    if coin_type.contains("sui::SUI") {
                                        // SUI 流出量 (Input + Gas)
                                        if amount_i128 < 0 {
                                            let total_out_abs = amount_i128.abs() as u64;
                                            // 如果流出量大於 Gas，代表多出來的是拿去 Swap 的
                                            if total_out_abs > net_gas_fee {
                                                swap_sui_in = (total_out_abs - net_gas_fee) as f64 / 1_000_000_000.0;
                                            }
                                        }
                                    } else if coin_type.contains("usdc::USDC") {
                                        // USDC 流入量
                                        if amount_i128 > 0 {
                                            swap_usdc_out = (amount_i128 as f64) / 1_000_000.0;
                                        }
                                    }
                                }
                            }
                        }

                        // 3. 計算真實價格與滑點
                        println!("   💰 gRPC 觸發價: {:.8}", trigger_price);
                        
                        if swap_sui_in > 0.0 {
                            let real_price = swap_usdc_out / swap_sui_in;
                            let diff_pct = ((real_price - trigger_price) / trigger_price) * 100.0;
                            
                            println!("   💵 實際成交價: {:.8} (Diff: {:.4}%)", real_price, diff_pct);
                            println!("   📉 真實投入: {:.4} SUI (已扣除 Gas)", swap_sui_in);
                            println!("   📈 實際獲得: {:.4} USDC", swap_usdc_out);

                            price_diff_result = diff_pct;
                        } else {
                            println!("   ⚠️ 無法還原 Swap 成本 (可能 Gas 佔比過高或資料異常)");
                        }
                        println!("   -----------------------------------------\n");

                        lag_result = lag;
                    }
                } else {
                    println!("⚠️ [分析] Tx: {} 尚未被索引或查詢失敗", tx_digest);
                }
            }
        }
        Err(e) => eprintln!("❌ [分析] 查詢 RPC 失敗: {}", e),
    }

    let _ = tx_sender.send(TradeStats {
        latency_ms,
        lag: lag_result,
        price_diff: price_diff_result,
    }).await;
}

// ✨ 修正版：遞迴搜尋 current_sqrt_price，不再受限於層級結構
fn extract_price_from_prost_struct(st: &prost_types::Struct) -> Option<String> {
    // 1. 先檢查當前這一層有沒有我們要的 key
    if let Some(val) = st.fields.get("current_sqrt_price") {
        if let Some(prost_types::value::Kind::StringValue(s)) = &val.kind {
            return Some(s.clone());
        }
    }

    // 2. 如果沒有，就遍歷所有欄位，如果是物件(Struct)就鑽進去找
    for (_, val) in &st.fields {
        if let Some(prost_types::value::Kind::StructValue(inner_st)) = &val.kind {
            if let Some(found) = extract_price_from_prost_struct(inner_st) {
                return Some(found);
            }
        }
    }
    
    None
}