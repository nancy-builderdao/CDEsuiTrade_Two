use std::error::Error;
use std::str::FromStr;

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
use futures::{StreamExt, SinkExt}; // WS 需要這兩個 trait
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use serde_json::Value; // 用來解析 WS JSON

mod bluefin;
// mod momentum; // 暫時註解掉沒用到的
// mod cetus;    // 暫時註解掉沒用到的

/// Enable / disable debug logs in main.rs.
const DEBUG_MAIN: bool = true;

/// Default swap amount.
const DEFAULT_SWAP_AMOUNT: u64 = 1000000;
const DEFAULT_GAS_BUDGET: u64 = 50_000_000;
const DEFAULT_GAS_PRICE: u64 = 1_000;

/// Private Key
const EXAMPLE_PRIVATE_KEY: &str = "suiprivkey1qzcq4jx6g0a8jmpwer0wfpr5kc8r2mfrmklj2a7f72xft2ff36w2wmsvyf4";

// ====== Bluefin specific constants ======
const BLUEFIN_GLOBAL_CONFIG_ID: &str = "0x03db251ba509a8d5d8777b6338836082335d93eecbdd09a11e190a1cff51c352";
const BLUEFIN_POOL_ID: &str = "0x15dbcac854b1fc68fc9467dbd9ab34270447aabd8cc0e04a5864d95ccb86b74a";
const BLUEFIN_TOKEN_OBJECT_ID: &str = "0x66bcedb93c0a58689944a5b8fb532e80c61300c8f8bf608f47d35dd0736c91b5"; // 假設這就是你要賣的 Token (Owned)

// Token types for Bluefin
const BLUEFIN_TOKEN_A_TYPE: &str = "0x2::sui::SUI";
const BLUEFIN_TOKEN_B_TYPE: &str = "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

/// 交易上下文：存放所有不需要去問節點的資料
#[derive(Debug, Clone)]
struct TradeContext {
    // Shared Object 的初始版本 (ISV)，永遠固定
    pool_isv: u64,
    global_config_isv: u64,
    clock_isv: u64,

    // 你的 Gas 物件 (啟動時查一次，之後自己維護)
    gas_object_id: Address,
    gas_version: u64,
    gas_digest: Digest,

    // 你要賣的 Token 物件 (啟動時查一次)
    token_object_id: Address,
    token_version: u64,
    token_digest: Digest,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 1. 初始化 HTTP Client (只用於預熱)
    // 建立一個負責發送交易的 Client
    let mut rpc_client = Client::new("http://3.114.103.176:443")?;
    
    // 解析 Private Key
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();
    println!("👤 Owner Address: {:?}", owner_address);

    // 2. 🔥 預熱：獲取所有需要的 Initial Version 和 Gas 資訊
    // 這一步會花一點時間，但只在程式啟動時做一次
    println!("🔥 正在預熱交易數據 (Fetching Object Details)...");
    let mut ctx = initialize_trade_context(&mut rpc_client, &owner_address).await?;
    println!("✅ 預熱完成！Context: {:#?}", ctx);

    // 3. 連線 WebSocket
    let ws_url = Url::parse("ws://3.114.103.176:9002/ws")?;
    println!("🔌 連線 WebSocket: {} ...", ws_url);

    let (ws_stream, _) = connect_async(ws_url).await?;
    println!("✅ WebSocket 已連線");

    let (mut write, mut read) = ws_stream.split();

    // 4. 訂閱 Pool (參照你的 python script)
    let subscribe_msg = serde_json::json!({
        "type": "subscribe_pool",
        "pool_id": BLUEFIN_POOL_ID
    });
    write.send(Message::Text(subscribe_msg.to_string())).await?;
    println!("Pw 訂閱請求已發送");

    println!("🚀 監控模式啟動，等待 WS 推播...");

    // 5. 進入 WS 監聽迴圈
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // 解析 JSON
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    let msg_type = json["type"].as_str().unwrap_or("");

                    if msg_type == "pool_update" {
                        let pool_id = json["pool_id"].as_str().unwrap_or("");
                        let version = json["version"].as_str().unwrap_or("N/A");
                        let digest = json["digest"].as_str().unwrap_or("N/A");

                        // 🔍 這裡就是你要的：收到資料就觸發
                        // 注意：我們不需要把 WS 裡的 version/digest 塞進 Context
                        // 因為 Pool 是 Shared Object，我們只用預熱拿到的 ISV
                        // WS 的資料僅用來判斷「價格」或「時機」
                        
                        println!("\n⚡️ 收到 Pool Update! ID: {} | Ver: {}", pool_id, version);
                        println!("   🔗 WS Digest: {}", digest);
                        
                        // 6. 🔫 觸發極速交易
                        // 這裡傳入 ctx，不需要再去問節點任何問題
                        match run_fast_swap(&mut rpc_client, &ctx, &private_key, owner_address).await {
                            Ok(_) => {
                                println!("✅ 交易發送成功！程式結束 (單次測試)");
                                break; // 測試只送一筆，避免連發
                            }
                            Err(e) => eprintln!("❌ 交易發送失敗: {}", e),
                        }
                    } else if msg_type == "SubscriptionSuccess" {
                        println!("✅ 訂閱成功: {:?}", json);
                    }
                }
            }
            Ok(_) => {}, // 忽略 Binary 或 Ping/Pong
            Err(e) => eprintln!("WS Error: {}", e),
        }
    }

    Ok(())
}

/// 核心函式：極速交易 (Zero-Lookup)
async fn run_fast_swap(
    client: &mut Client,
    ctx: &TradeContext,
    signer_key: &Ed25519PrivateKey,
    owner: Address,
) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();

    // 1. 直接從 Context 建立 Input
    // ⚠️ 這裡直接用記憶體裡的資料，沒有任何 await/fetch

    // Gas Input (Owned)
    let gas_input = Input::by_id(ctx.gas_object_id)
        .with_owned_kind()
        .with_version(ctx.gas_version)
        .with_digest(ctx.gas_digest);

    // Token Input (Owned)
    let token_input = Input::by_id(ctx.token_object_id)
        .with_owned_kind()
        .with_version(ctx.token_version)
        .with_digest(ctx.token_digest);

    // Pool Input (Shared) - 這裡一定要用 ISV，不能用 WS 推過來的最新 Version
    let pool_input = Input::by_id(Address::from_str(BLUEFIN_POOL_ID)?)
        .with_shared_kind()
        .with_initial_shared_version(ctx.pool_isv)
        .by_val();

    // Config Input (Shared)
    let global_config_input = Input::by_id(Address::from_str(BLUEFIN_GLOBAL_CONFIG_ID)?)
        .with_shared_kind()
        .with_initial_shared_version(ctx.global_config_isv)
        .by_ref();

    // Clock Input (Shared)
    let clock_input = Input::by_id(Address::from_str("0x6")?)
        .with_shared_kind()
        .with_initial_shared_version(ctx.clock_isv)
        .by_ref();

    // 2. 建構交易
    let amount_in = DEFAULT_SWAP_AMOUNT;
    let a2b = true; // SUI -> USDC

    // debug_main("Building transaction...");
    let tx = bluefin::create_bluefin_swap_transaction(
        token_input, pool_input, global_config_input, clock_input, gas_input,
        amount_in, a2b, owner, 
        DEFAULT_GAS_BUDGET, DEFAULT_GAS_PRICE,
        BLUEFIN_TOKEN_A_TYPE, BLUEFIN_TOKEN_B_TYPE,
    )?;

    // 3. 簽名
    let signature = signer_key.sign_transaction(&tx)?;

    // 4. 發送 (只有這一步會跟網路互動)
    let mut request = sui_rpc::proto::sui::rpc::v2::ExecuteTransactionRequest::default();
    request.transaction = Some(tx.into());
    request.signatures = vec![signature.into()];

    let response = client.execution_client().execute_transaction(request).await?;
    let elapsed = start.elapsed();
    
    let resp_inner = response.into_inner();
    
    // 抓取 Digest
    let tx_digest = resp_inner.transaction.as_ref()
        .and_then(|t| t.effects.as_ref()) 
        .and_then(|e| e.transaction_digest.as_ref()) 
        .map(|d| d.to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    println!(
        "🚀 Tx Sent! Digest: {} | ⏱️ Latency: {:.3?}",
        tx_digest, elapsed
    );

    // 這裡我們沒有處理 Gas Update，因為你說先送一筆就好
    
    Ok(())
}


/// 預熱函式：一次性抓取所有需要的資料
async fn initialize_trade_context(
    client: &mut Client, 
    owner: &Address
) -> Result<TradeContext, Box<dyn Error>> {
    // 1. 抓 Shared Object 的 ISV
    let pool_id: Address = BLUEFIN_POOL_ID.parse()?;
    let config_id: Address = BLUEFIN_GLOBAL_CONFIG_ID.parse()?;
    let clock_id: Address = "0x6".parse()?;

    let pool_obj = fetch_object_details(client, pool_id).await?;
    let config_obj = fetch_object_details(client, config_id).await?;
    let clock_obj = fetch_object_details(client, clock_id).await?;

    // 2. 抓 Gas (Owned)
    let gas_id = fetch_first_sui_gas_object_id(client, owner).await?;
    let gas_obj = fetch_object_details(client, gas_id).await?;

    // 3. 抓 Token (Owned)
    let token_id: Address = BLUEFIN_TOKEN_OBJECT_ID.parse()?;
    let token_obj = fetch_object_details(client, token_id).await?;

    Ok(TradeContext {
        pool_isv: get_initial_shared_version(&pool_obj)?,
        global_config_isv: get_initial_shared_version(&config_obj)?,
        clock_isv: get_initial_shared_version(&clock_obj)?,

        gas_object_id: gas_id,
        gas_version: gas_obj.version.ok_or("No Gas Ver")?,
        gas_digest: gas_obj.digest.ok_or("No Gas Digest")?.parse()?,

        token_object_id: token_id,
        token_version: token_obj.version.ok_or("No Token Ver")?,
        token_digest: token_obj.digest.ok_or("No Token Digest")?.parse()?,
    })
}

// ---------------- Helper Functions (保持原樣或微調) ----------------

fn decode_sui_private_key(key_str: &str) -> Result<Ed25519PrivateKey, Box<dyn Error>> {
    let (_hrp, data, _variant) = bech32::decode(key_str)?;
    let bytes = Vec::<u8>::from_base32(&data)?;
    if bytes.len() != 33 || bytes[0] != 0 {
        return Err("Invalid Sui private key format".into());
    }
    let pk_bytes: [u8; 32] = bytes[1..].try_into().map_err(|_| "Invalid Key Length")?;
    Ok(Ed25519PrivateKey::new(pk_bytes))
}

async fn fetch_first_sui_gas_object_id(
    client: &mut Client,
    owner: &Address,
) -> Result<Address, Box<dyn Error>> {
    let mut state_client = client.state_client();
    let mut request = ListOwnedObjectsRequest::default();
    request.owner = Some(owner.to_string());
    request.object_type = Some("0x2::coin::Coin<0x2::sui::SUI>".to_string());
    request.read_mask = Some(FieldMask { paths: vec!["object_id".to_string()] });

    let response = state_client.list_owned_objects(request).await?.into_inner();
    if response.objects.is_empty() {
        return Err("No SUI gas objects found".into());
    }
    let oid_str = response.objects[0].object_id.as_ref().ok_or("Missing object_id")?;
    Ok(oid_str.parse()?)
}

async fn fetch_object_details(
    client: &mut Client,
    object_id: Address,
) -> Result<Object, Box<dyn Error>> {
    let mut ledger_client = client.ledger_client();
    let mut request = GetObjectRequest::new(&object_id);
    request.read_mask = Some(FieldMask {
        paths: vec!["object_id".to_string(), "version".to_string(), "digest".to_string(), "owner".to_string()],
    });
    let response = ledger_client.get_object(request).await?.into_inner();
    response.object.ok_or_else(|| "Object not found".into())
}

fn get_initial_shared_version(obj: &Object) -> Result<u64, Box<dyn Error>> {
    if let Some(ref owner) = obj.owner {
        return Ok(owner.version());
    }
    Err("Object is not shared".into())
}

fn debug_main(msg: &str) {
    if DEBUG_MAIN { eprintln!("{msg}"); }
}