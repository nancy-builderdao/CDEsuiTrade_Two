use std::error::Error;
use std::str::FromStr;

use bech32::FromBase32;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_crypto::SuiSigner;
use sui_rpc::Client;
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

mod bluefin;

const DEBUG_MAIN: bool = true;

const DEFAULT_SWAP_AMOUNT: u64 = 1000000;
const DEFAULT_GAS_BUDGET: u64 = 50_000_000;
const DEFAULT_GAS_PRICE: u64 = 1_000;

const EXAMPLE_PRIVATE_KEY: &str = "suiprivkey1qzcq4jx6g0a8jmpwer0wfpr5kc8r2mfrmklj2a7f72xft2ff36w2wmsvyf4";

// ====== Bluefin constants ======
const BLUEFIN_GLOBAL_CONFIG_ID: &str = "0x03db251ba509a8d5d8777b6338836082335d93eecbdd09a11e190a1cff51c352";
const BLUEFIN_POOL_ID: &str = "0x15dbcac854b1fc68fc9467dbd9ab34270447aabd8cc0e04a5864d95ccb86b74a";
const BLUEFIN_TOKEN_OBJECT_ID: &str = "0x66bcedb93c0a58689944a5b8fb532e80c61300c8f8bf608f47d35dd0736c91b5";

const BLUEFIN_TOKEN_A_TYPE: &str = "0x2::sui::SUI";
const BLUEFIN_TOKEN_B_TYPE: &str = "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

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
    let mut rpc_client = Client::new("http://3.114.103.176:443")?;
    
    let private_key = decode_sui_private_key(EXAMPLE_PRIVATE_KEY)?;
    let public_key = private_key.public_key();
    let owner_address = public_key.derive_address();
    println!("👤 Owner Address: {:?}", owner_address);

    println!("🔥 正在預熱交易數據 (Fetching Object Details)...");
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
    println!("Pw 訂閱請求已發送");

    println!("🚀 監控模式啟動，等待 WS 推播...");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    let msg_type = json["type"].as_str().unwrap_or("");

                    if msg_type == "pool_update" {
                        let _pool_id = json["pool_id"].as_str().unwrap_or("Unknown");
                        let version = json["version"].as_u64().map(|v| v.to_string())
                            .or_else(|| json["version"].as_str().map(|s| s.to_string()))
                            .unwrap_or("N/A".to_string());
                        
                        let trigger_digest = json["digest"].as_str().unwrap_or("Unknown").to_string();

                        let mut price_display = "N/A".to_string();
                        let mut ws_price_f64 = 0.0;

                        if let Some(obj_array) = json["object"].as_array() {
                            let raw_bytes: Vec<u8> = obj_array.iter()
                                .map(|v| v.as_u64().unwrap_or(0) as u8)
                                .collect();
                            
                            if let Some(price) = get_bluefin_price(&raw_bytes) {
                                ws_price_f64 = price;
                                price_display = format!("{:.8}", price);
                            }
                        }

                        println!("\n⚡️ Pool Update! Ver: {}", version);
                        println!("   🔗 Trigger Digest: {}", trigger_digest);
                        println!("   💰 WS Sort Price: {}", price_display);

                        match run_fast_swap(&mut rpc_client, &ctx, &private_key, owner_address, ws_price_f64, trigger_digest).await {
                            Ok(_) => {
                                println!("✅ 交易發送成功！程式結束 (單次測試)");
                                break; 
                            }
                            Err(e) => eprintln!("❌ 交易發送失敗: {}", e),
                        }
                    } else if msg_type == "SubscriptionSuccess" {
                        println!("✅ 訂閱成功");
                    }
                }
            }
            Ok(_) => {},
            Err(e) => eprintln!("WS Error: {}", e),
        }
    }

    println!("⏳ 測試模式：主程式等待 10 秒讓背景分析完成...");
    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

    Ok(())
}

fn get_bluefin_price(data: &[u8]) -> Option<f64> {
    let offset = 279;
    if data.len() < offset + 16 {
        return None;
    }
    let chunk = &data[offset..offset+16];
    let low = u64::from_le_bytes(chunk[0..8].try_into().ok()?);
    let high = u64::from_le_bytes(chunk[8..16].try_into().ok()?);
    let sqrt_price = ((high as u128) << 64) | (low as u128);

    let multiplier = 1000.0;
    let sqrt_price_f = sqrt_price as f64;
    let denom = (1u128 << 64) as f64; 
    
    let raw_price = (sqrt_price_f / denom).powi(2);
    let sort_price = raw_price * multiplier;

    Some(sort_price)
}

async fn run_fast_swap(
    client: &mut Client,
    ctx: &TradeContext,
    signer_key: &Ed25519PrivateKey,
    owner: Address,
    ws_price: f64,
    trigger_digest: String,
) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();

    let gas_input = Input::by_id(ctx.gas_object_id)
        .with_owned_kind()
        .with_version(ctx.gas_version)
        .with_digest(ctx.gas_digest);

    let token_input = Input::by_id(ctx.token_object_id)
        .with_owned_kind()
        .with_version(ctx.token_version)
        .with_digest(ctx.token_digest);

    let pool_input = Input::by_id(Address::from_str(BLUEFIN_POOL_ID)?)
        .with_shared_kind()
        .with_initial_shared_version(ctx.pool_isv)
        .by_val();

    let global_config_input = Input::by_id(Address::from_str(BLUEFIN_GLOBAL_CONFIG_ID)?)
        .with_shared_kind()
        .with_initial_shared_version(ctx.global_config_isv)
        .by_ref();

    let clock_input = Input::by_id(Address::from_str("0x6")?)
        .with_shared_kind()
        .with_initial_shared_version(ctx.clock_isv)
        .by_ref();

    let amount_in = DEFAULT_SWAP_AMOUNT;
    let a2b = true; 

    let tx = bluefin::create_bluefin_swap_transaction(
        token_input, pool_input, global_config_input, clock_input, gas_input,
        amount_in, a2b, owner, 
        DEFAULT_GAS_BUDGET, DEFAULT_GAS_PRICE,
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

    println!(
        "🚀 Tx Sent! Digest: {} | ⏱️ Latency: {:.3?}",
        tx_digest, elapsed
    );

    if tx_digest != "Unknown" {
        let digest_clone = tx_digest.clone();
        let rpc_url = "http://3.114.103.176:443".to_string();
        
        tokio::spawn(async move {
            analyze_trade_result(rpc_url, digest_clone, trigger_digest, ws_price).await;
        });
    }

    Ok(())
}

async fn analyze_trade_result(
    rpc_url: String, 
    digest: String,
    trigger_digest: String,
    ws_price: f64,
) {
    let mut client = match Client::new(&rpc_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ [Analysis] 無法建立 Client: {}", e);
            return;
        }
    };
    
    let mut ledger_client = client.ledger_client();
    println!("   ... 正在背景追蹤交易 (Trigger: {} -> Tx: {})", trigger_digest, digest);

    // 暫時跳過 Checkpoint 查詢，專注於解析金額

    for _ in 1..=20 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let mut request = GetTransactionRequest::default();
        request.digest = Some(digest.clone());
        request.read_mask = Some(FieldMask {
            paths: vec![
                "transaction".to_string(),
                "effects".to_string(), 
                "balance_changes".to_string(),
                // "checkpoint_sequence_number".to_string() // 暫時移除以通過編譯
            ],
        });

        if let Ok(response) = ledger_client.get_transaction(request).await {
            let resp = response.into_inner();
            
            // 直接解包 transaction
            if let Some(tx_block) = resp.transaction {
                
                // 確認已執行 (檢查 effects)
                if let Some(effects) = &tx_block.effects {
                    
                    println!("\n📊 [交易分析報告] {}", digest);
                    println!("   -----------------------------------------");
                    println!("   ✅ 交易已上鏈 (Confirmed)");
                    
                    println!("   💰 WS 觸發價: {:.8}", ws_price);

                    let mut net_gas_fee: u64 = 0;
                    
                    // 計算 Gas Fee
                    if let Some(gas_summary) = &effects.gas_used {
                        let comp = gas_summary.computation_cost.unwrap_or(0);
                        let storage = gas_summary.storage_cost.unwrap_or(0);
                        let rebate = gas_summary.storage_rebate.unwrap_or(0);
                        
                        let total_cost = comp + storage;
                        if total_cost > rebate {
                            net_gas_fee = total_cost - rebate;
                        }
                    }

                    let mut swap_sui_in = 0.0;
                    let mut swap_usdc_out = 0.0;
                    
                    // 修正：直接迭代 Vec (解決 mismatched types 錯誤)
                    for change in tx_block.balance_changes {
                         let coin_type = change.coin_type.unwrap_or_default();
                         let amount_str = change.amount.unwrap_or_default();
                         
                         // 明確指定型別為 i128
                         if let Ok(amount_i128) = amount_str.parse::<i128>() {
                             
                             if coin_type.contains("sui::SUI") {
                                 // SUI 流出量 (Input + Gas)
                                 if amount_i128 < 0 {
                                     // 修正：明確指定 abs() 運算後的型別 (解決 type annotation 錯誤)
                                     let total_out_abs: i128 = amount_i128.abs();
                                     let total_out = total_out_abs as u64;

                                     if total_out > net_gas_fee {
                                         // 還原真實 Swap 投入
                                         swap_sui_in = (total_out - net_gas_fee) as f64 / 1_000_000_000.0;
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

                    if swap_sui_in > 0.0 {
                        let real_price = swap_usdc_out / swap_sui_in;
                        let diff_pct = ((real_price - ws_price) / ws_price) * 100.0;
                        println!("   💵 實際成交價: {:.8} (Diff: {:.4}%)", real_price, diff_pct);
                        println!("   📉 真實投入: {:.4} SUI (已扣除 Gas: {:.4})", swap_sui_in, net_gas_fee as f64 / 1_000_000_000.0);
                        println!("   📈 實際獲得: {:.4} USDC", swap_usdc_out);
                    } else {
                        println!("   ⚠️ 無法還原 Swap 成本 (Gas 佔比過高或資料異常)");
                    }

                    println!("   -----------------------------------------\n");
                    return;
                }
            }
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