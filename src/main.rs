use std::error::Error;
use std::str::FromStr;
use std::process::Command;

use bech32::FromBase32;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_crypto::SuiSigner;
use sui_rpc::Client;
// 這裡保留需要的 import，移除不需要的以避免警告
use sui_rpc::proto::sui::rpc::v2::{
    ListOwnedObjectsRequest, GetObjectRequest, GetTransactionRequest
};
use sui_sdk_types::{Address, Digest};
use sui_transaction_builder::unresolved::Input;
use prost_types::FieldMask;
use sui_rpc::proto::sui::rpc::v2::Object;
use tokio::time::Instant;
use futures::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use serde_json::Value;
use tokio::sync::oneshot; // ✨ 新增：用於通知主程式任務完成

mod bluefin;

const DEBUG_MAIN: bool = true;

const DEFAULT_SWAP_AMOUNT: u64 = 1000000;
const DEFAULT_GAS_BUDGET: u64 = 50_000_000;
const DEFAULT_GAS_PRICE: u64 = 1_000;

const EXAMPLE_PRIVATE_KEY: &str = "suiprivkey1qzcq4jx6g0a8jmpwer0wfpr5kc8r2mfrmklj2a7f72xft2ff36w2wmsvyf4";

// ====== Bluefin constants ======
const BLUEFIN_GLOBAL_CONFIG_ID: &str = "0x03db251ba509a8d5d8777b6338836082335d93eecbdd09a11e190a1cff51c352";
const BLUEFIN_POOL_ID: &str = "0x15dbcac854b1fc68fc9467dbd9ab34270447aabd8cc0e04a5864d95ccb86b74a";

const BLUEFIN_TOKEN_A_TYPE: &str = "0x2::sui::SUI";
const BLUEFIN_TOKEN_B_TYPE: &str = "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

// ✨ 修改：改用 Localhost，確保防火牆不會擋，且速度最快
const JSON_RPC_URL: &str = "http://3.114.103.176:443"; 

#[derive(Debug, Clone)]
struct TradeContext {
    pool_isv: u64,
    global_config_isv: u64,
    clock_isv: u64,
    gas_object_id: Address,
    gas_version: u64,
    gas_digest: Digest,
    token_object_id: Address,
    token_version: u64,
    token_digest: Digest,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut rpc_client = Client::new("http://3.114.103.176:443")?; // 這裡可以是公網，但 JSON-RPC 用本地
    
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();
    println!("👤 Owner Address: {:?}", owner_address);

    for round in 1..=10 {
        println!("\n========================================");
        println!("🔄 第 {} / 10 次執行開始", round);
        println!("========================================");

        println!("🔥 正在預熱交易數據...");
        let ctx = initialize_trade_context(&mut rpc_client, &owner_address).await?;
        println!("✅ 預熱完成！Pool ISV: {}", ctx.pool_isv);

        let ws_url = Url::parse("ws://3.114.103.176:9002/ws")?;
        println!("🔌 連線 WebSocket: {} ...", ws_url);
        let (ws_stream, _) = connect_async(ws_url).await?;
        println!("✅ WebSocket 已連線");

        let (mut write, mut read) = ws_stream.split();

        let subscribe_msg = serde_json::json!({
            "type": "subscribe_pool",
            "pool_id": BLUEFIN_POOL_ID
        });
        write.send(Message::Text(subscribe_msg.to_string())).await?;
        println!("🚀 監控模式啟動，等待 WS 推播...");

        // 建立一個通道，讓背景任務通知主程式「我做完了」
        let (tx_done, rx_done) = oneshot::channel();
        let mut tx_done_opt = Some(tx_done); // Option wrap 避免多次移動

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(json) = serde_json::from_str::<Value>(&text) {
                        if json["type"].as_str() == Some("pool_update") {
                            let version = json["version"].as_u64().map(|v| v.to_string()).unwrap_or("N/A".to_string());
                            let trigger_digest = json["digest"].as_str().unwrap_or("Unknown").to_string();
                            
                            // 解析價格顯示
                            let mut price_display = "N/A".to_string();
                            let mut ws_price_f64 = 0.0;
                            if let Some(obj_array) = json["object"].as_array() {
                                let raw_bytes: Vec<u8> = obj_array.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                                if let Some(price) = get_bluefin_price(&raw_bytes) {
                                    ws_price_f64 = price;
                                    price_display = format!("{:.8}", price);
                                }
                            }

                            println!("\n⚡️ Pool Update! Ver: {}", version);
                            println!("   🔗 Trigger Digest: {}", trigger_digest);
                            println!("   💰 WS Sort Price: {}", price_display);

                            // 觸發交易，並傳入通知通道
                            if let Some(done_sender) = tx_done_opt.take() {
                                match run_fast_swap(&mut rpc_client, &ctx, &private_key, owner_address, ws_price_f64, trigger_digest, done_sender).await {
                                    Ok(_) => {
                                        println!("✅ 交易發送成功！等待背景分析...");
                                        break; // 跳出 WS 迴圈，進入等待模式
                                    }
                                    Err(e) => eprintln!("❌ 交易發送失敗: {}", e),
                                }
                            }
                        } else if json["type"].as_str() == Some("SubscriptionSuccess") {
                            println!("✅ 訂閱成功");
                        }
                    }
                }
                Ok(_) => {},
                Err(e) => eprintln!("WS Error: {}", e),
            }
        }

        // 主程式在此等待背景任務完成 (最多等 10 秒)
        println!("⏳ 主程式等待分析報告中 (Timeout: 10s)...");
        match tokio::time::timeout(tokio::time::Duration::from_secs(10), rx_done).await {
            Ok(_) => println!("✅ 分析完成，程式正常結束。"),
            Err(_) => println!("⚠️ 等待逾時：背景分析可能卡住或失敗。"),
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
    println!("🎉 全部 10 次執行完畢！");

    Ok(())
}

fn get_bluefin_price(data: &[u8]) -> Option<f64> {
    let offset = 279;
    if data.len() < offset + 16 { return None; }
    let chunk = &data[offset..offset+16];
    let low = u64::from_le_bytes(chunk[0..8].try_into().ok()?);
    let high = u64::from_le_bytes(chunk[8..16].try_into().ok()?);
    let sqrt_price = ((high as u128) << 64) | (low as u128);
    let multiplier = 1000.0;
    let denom = (1u128 << 64) as f64; 
    let raw_price = (sqrt_price as f64 / denom).powi(2);
    Some(raw_price * multiplier)
}

async fn run_fast_swap(
    client: &mut Client,
    ctx: &TradeContext,
    signer_key: &Ed25519PrivateKey,
    owner: Address,
    ws_price: f64,
    trigger_digest: String,
    done_signal: oneshot::Sender<()>, // ✨ 傳入通道
) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();

    let gas_input = Input::by_id(ctx.gas_object_id).with_owned_kind().with_version(ctx.gas_version).with_digest(ctx.gas_digest);
    let token_input = Input::by_id(ctx.token_object_id).with_owned_kind().with_version(ctx.token_version).with_digest(ctx.token_digest);
    let pool_input = Input::by_id(Address::from_str(BLUEFIN_POOL_ID)?).with_shared_kind().with_initial_shared_version(ctx.pool_isv).by_val();
    let global_config_input = Input::by_id(Address::from_str(BLUEFIN_GLOBAL_CONFIG_ID)?).with_shared_kind().with_initial_shared_version(ctx.global_config_isv).by_ref();
    let clock_input = Input::by_id(Address::from_str("0x6")?).with_shared_kind().with_initial_shared_version(ctx.clock_isv).by_ref();

    let tx = bluefin::create_bluefin_swap_transaction(
        token_input, pool_input, global_config_input, clock_input, gas_input,
        DEFAULT_SWAP_AMOUNT, true, owner, DEFAULT_GAS_BUDGET, DEFAULT_GAS_PRICE,
        BLUEFIN_TOKEN_A_TYPE, BLUEFIN_TOKEN_B_TYPE,
    )?;

    let signature = signer_key.sign_transaction(&tx)?;
    let mut request = sui_rpc::proto::sui::rpc::v2::ExecuteTransactionRequest::default();
    request.transaction = Some(tx.into());
    request.signatures = vec![signature.into()];

    let response = client.execution_client().execute_transaction(request).await?;
    let elapsed = start.elapsed();
    let resp_inner = response.into_inner();
    
    let tx_digest = resp_inner.transaction.as_ref()
        .and_then(|t| t.effects.as_ref()) 
        .and_then(|e| e.transaction_digest.as_ref()) 
        .map(|d| d.to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    println!("🚀 Tx Sent! Digest: {} | ⏱️ Latency: {:.3?}", tx_digest, elapsed);

    if tx_digest != "Unknown" {
        let digest_clone = tx_digest.clone();
        
        // Spawn 背景任務
        tokio::spawn(async move {
            analyze_trade_result(digest_clone, trigger_digest, ws_price).await;
            // 通知主程式：我做完了
            let _ = done_signal.send(());
        });
    } else {
        // 如果失敗，也要通知主程式不要空等
        let _ = done_signal.send(());
    }

    Ok(())
}

/// 使用 curl 呼叫 JSON-RPC，並增加錯誤日誌
fn fetch_tx_info_via_curl(digest: &str) -> Option<(u64, Value)> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "sui_getTransactionBlock",
        "params": [
            digest,
            {
                "showInput": false, "showRawInput": false, "showEffects": true,
                "showEvents": false, "showObjectChanges": false, "showBalanceChanges": true
            }
        ]
    });

    // 呼叫 curl
    let output = Command::new("curl")
        .arg("-s")
        .arg("-X").arg("POST")
        .arg("-H").arg("Content-Type: application/json")
        .arg("-d").arg(payload.to_string())
        .arg(JSON_RPC_URL) // 使用 Localhost
        .output()
        .ok()?;

    if !output.status.success() {
        eprintln!("❌ Curl failed with status: {:?}", output.status);
        return None;
    }

    let resp_text = String::from_utf8(output.stdout).ok()?;
    
    // 嘗試解析 JSON
    let json: Value = match serde_json::from_str(&resp_text) {
        Ok(v) => v,
        Err(_) => {
            // 如果解析失敗，印出原始文字看看是不是 Nginx 錯誤或空值
            // eprintln!("❌ JSON Parse Error. Raw: {}", resp_text);
            return None;
        }
    };

    if let Some(err) = json.get("error") {
        // 這是正常的，代表還沒查到 (Not found)
        // eprintln!("⚠️ RPC Error: {:?}", err); 
        return None;
    }

    let checkpoint = json["result"]["checkpoint"].as_str()
        .and_then(|s| s.parse::<u64>().ok());

    if let Some(cp) = checkpoint {
        Some((cp, json["result"].clone()))
    } else {
        None
    }
}

async fn analyze_trade_result(
    digest: String,
    trigger_digest: String,
    ws_price: f64,
) {
    println!("   ... 正在背景追蹤交易 (Trigger: {} -> Tx: {})", trigger_digest, digest);

    // 1. 查 Trigger Checkpoint
    let mut trigger_cp = 0;
    println!("   ... [1/2] 查詢 Trigger CP ...");
    for _ in 0..10 { // 試 5 秒
        if let Some((cp, _)) = fetch_tx_info_via_curl(&trigger_digest) {
            trigger_cp = cp;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    if trigger_cp == 0 {
        println!("   ⚠️ 無法查到 Trigger CP (可能節點尚未索引 WS 推播的交易)");
    }

    // 2. 查 User Tx
    println!("   ... [2/2] 查詢 My Tx CP ...");
    for i in 1..=40 { // 延長到 20 秒
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        if let Some((exec_cp, result)) = fetch_tx_info_via_curl(&digest) {
             println!("\n📊 [交易分析報告] {}", digest);
             println!("   -----------------------------------------");
             
             if trigger_cp > 0 {
                 let diff = exec_cp as i64 - trigger_cp as i64;
                 println!("   ⏱️ 區塊延遲: {} blocks (Trigger: {} -> Exec: {})", diff, trigger_cp, exec_cp);
             } else {
                 println!("   ⏱️ 區塊延遲: 未知 (Trigger未查到) -> Exec: {}", exec_cp);
             }
             
             println!("   💰 WS 觸發價: {:.8}", ws_price);

             // 解析真實成本
             let mut net_gas_fee: u64 = 0;
             if let Some(gas_summary) = result["effects"]["gasUsed"].as_object() {
                 let comp = gas_summary["computationCost"].as_str().unwrap_or("0").parse::<u64>().unwrap_or(0);
                 let storage = gas_summary["storageCost"].as_str().unwrap_or("0").parse::<u64>().unwrap_or(0);
                 let rebate = gas_summary["storageRebate"].as_str().unwrap_or("0").parse::<u64>().unwrap_or(0);
                 let total_cost = comp + storage;
                 if total_cost > rebate { net_gas_fee = total_cost - rebate; }
             }

             let mut swap_sui_in = 0.0;
             let mut swap_usdc_out = 0.0;

             if let Some(changes) = result["balanceChanges"].as_array() {
                 for change in changes {
                     let coin_type = change["coinType"].as_str().unwrap_or("");
                     let amount_str = change["amount"].as_str().unwrap_or("0");
                     
                     if let Ok(amount_i128) = amount_str.parse::<i128>() {
                         if coin_type.contains("sui::SUI") {
                             if amount_i128 < 0 {
                                 let total_out_abs = amount_i128.abs() as u64;
                                 if total_out_abs > net_gas_fee {
                                     swap_sui_in = (total_out_abs - net_gas_fee) as f64 / 1_000_000_000.0;
                                 }
                             }
                         } else if coin_type.contains("usdc::USDC") {
                             if amount_i128 > 0 {
                                 swap_usdc_out = (amount_i128 as f64) / 1_000_000.0;
                             }
                         }
                     }
                 }
             }

             if swap_sui_in > 0.0 {
                 let real_price = swap_usdc_out / swap_sui_in;
                 let diff_pct = ((real_price - ws_price) / ws_price) * 100.0;
                 println!("   💵 實際成交價: {:.8} (Diff: {:.4}%)", real_price, diff_pct);
                 println!("   📉 真實投入: {:.4} SUI", swap_sui_in);
                 println!("   📈 實際獲得: {:.4} USDC", swap_usdc_out);
             } else {
                 println!("   ⚠️ 無法還原 Swap 成本 (可能餘額變動過小)");
             }
             println!("   -----------------------------------------\n");
             return;
        }
        
        // 進度顯示
        if i % 5 == 0 {
            println!("   ... 正在等待節點索引 ({}s)...", i / 2);
        }
    }
    println!("⚠️ [Analysis] 交易 {} 查詢超時", digest);
}

// === 初始化函式 (保持不變) ===
async fn initialize_trade_context(
    client: &mut Client, 
    owner: &Address
) -> Result<TradeContext, Box<dyn Error>> {
    let pool_id: Address = BLUEFIN_POOL_ID.parse()?;
    let config_id: Address = BLUEFIN_GLOBAL_CONFIG_ID.parse()?;
    let clock_id: Address = "0x6".parse()?;

    let pool_obj = fetch_object_details(client, pool_id).await?;
    let config_obj = fetch_object_details(client, config_id).await?;
    let clock_obj = fetch_object_details(client, clock_id).await?;

    let gas_id = fetch_first_sui_gas_object_id(client, owner).await?;
    let gas_obj = fetch_object_details(client, gas_id).await?;

    let token_id = fetch_sui_coin_excluding_gas(client, owner, gas_id).await?; 
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

fn decode_sui_private_key(key_str: &str) -> Result<Ed25519PrivateKey, Box<dyn Error>> {
    let (_hrp, data, _variant) = bech32::decode(key_str)?;
    let bytes = Vec::<u8>::from_base32(&data)?;
    if bytes.len() != 33 || bytes[0] != 0 { return Err("Invalid Sui private key".into()); }
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
    if response.objects.is_empty() { return Err("No SUI gas objects found".into()); }
    let oid_str = response.objects[0].object_id.as_ref().ok_or("Missing object_id")?;
    Ok(oid_str.parse()?)
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
    if let Some(ref owner) = obj.owner { return Ok(owner.version()); }
    Err("Object is not shared".into())
}