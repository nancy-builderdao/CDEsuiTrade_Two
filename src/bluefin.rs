use std::error::Error;

use sui_sdk_types::{Address, Argument, TypeTag};
use sui_transaction_builder::{Function, Serialized, TransactionBuilder};
use sui_transaction_builder::unresolved::Input;

/// Enable / disable debug logs inside bluefin module.
const DEBUG_BLUEFIN: bool = true;

// ====== Bluefin constants ======
const BLUEFIN_GATEWAY_PACKAGE: &str =
    "0xd075338d105482f1527cbfd363d6413558f184dec36d9138a70261e87f486e9c";
const MODULE_NAME: &str = "gateway";
const SWAP_FUNCTION_NAME: &str = "swap_assets";

// price limits
const MAX_SQRT_PRICE: u128 = 79_226_673_515_401_279_992_447_579_054u128;
const MIN_SQRT_PRICE: u128 = 4_295_048_017u128;

/// Build a Bluefin swap transaction using Input objects (same pattern as momentum.rs).
///
/// - `token_input`: input for the token coin (owned object).
/// - `pool_input`: input for the pool shared object.
/// - `global_config_input`: input for the global config shared object.
/// - `clock_input`: input for the clock shared object.
/// - `gas_input`: input for the gas coin object.
/// - `amount_in`: input token amount.
/// - `a2b`: true for A -> B (SUI -> USDC), false for B -> A (USDC -> SUI).
/// - `sender`: transaction sender.
/// - `gas_budget`, `gas_price`: gas configuration.
/// - `token_a_type`, `token_b_type`: token type strings.
pub fn create_bluefin_swap_transaction(
    token_input: Input,
    pool_input: Input,
    global_config_input: Input,
    clock_input: Input,
    gas_input: Input,
    amount_in: u64,
    a2b: bool,
    sender: Address,
    gas_budget: u64,
    gas_price: u64,
    token_a_type: &str,
    token_b_type: &str,
) -> Result<sui_sdk_types::Transaction, Box<dyn Error>> {
    debug_bluefin("[create_bluefin_swap_transaction] start");

    let mut tx = TransactionBuilder::new();

    // Transaction metadata.
    tx.set_sender(sender);
    tx.set_gas_budget(gas_budget);
    tx.set_gas_price(gas_price);

    debug_bluefin(&format!(
        "[create_bluefin_swap_transaction] sender={sender}, gas_budget={gas_budget}, gas_price={gas_price}"
    ));

    // Gas input.
    debug_bluefin(&format!(
        "[create_bluefin_swap_transaction] adding gas object: {gas_input:?}"
    ));
    tx.add_gas_objects(vec![gas_input]);

    // Add inputs to transaction.
    debug_bluefin("[create_bluefin_swap_transaction] adding token, pool, global_config, clock inputs");
    let token_in = tx.input(token_input);
    let global_cfg = tx.input(global_config_input);
    let pool = tx.input(pool_input);
    let clock = tx.input(clock_input);

    // Build swap logic.
    debug_bluefin("[create_bluefin_swap_transaction] before bluefin_swap()");
    bluefin_swap(
        &mut tx,
        token_in,
        pool,
        global_cfg,
        clock,
        amount_in,
        a2b,
        token_a_type,
        token_b_type,
        sender,
    )?;
    debug_bluefin("[create_bluefin_swap_transaction] after bluefin_swap()");

    // Finalize transaction.
    debug_bluefin("[create_bluefin_swap_transaction] before finish()");
    let transaction = tx.finish()?;
    debug_bluefin("[create_bluefin_swap_transaction] after finish()");

    Ok(transaction)
}

/// Core Bluefin swap logic built on top of TransactionBuilder.
fn bluefin_swap(
    tx: &mut TransactionBuilder,
    token_in: Argument,
    pool: Argument,
    global_cfg: Argument,
    clock: Argument,
    amount_in: u64,
    a2b: bool,
    token_a_type: &str,
    token_b_type: &str,
    sender: Address,
) -> Result<(), Box<dyn Error>> {
    debug_bluefin(&format!(
        "[bluefin_swap] start, amount_in={amount_in}, a2b={a2b}"
    ));

    let token_a_tag: TypeTag = token_a_type.parse()?;
    let token_b_tag: TypeTag = token_b_type.parse()?;

    // Pure args.
    let a2b_arg = tx.input(Serialized(&a2b));
    let by_amount_in_arg = tx.input(Serialized(&true));
    let amount_arg = tx.input(Serialized(&amount_in));
    let timestamp_arg = tx.input(Serialized(&0u64));

    let sqrt_price_limit: u128 = if a2b { MIN_SQRT_PRICE } else { MAX_SQRT_PRICE };
    let sqrt_price_limit_arg = tx.input(Serialized(&sqrt_price_limit));

    debug_bluefin(&format!(
        "[bluefin_swap] sqrt_price_limit={sqrt_price_limit}"
    ));

    // Step 1: Create zero coin for output side.
    debug_bluefin("[bluefin_swap] step 1: create zero coin for output");
    let zero_coin = if a2b {
        tx.move_call(
            Function::new(
                "0x2".parse()?,
                "coin".parse()?,
                "zero".parse()?,
                vec![token_b_tag.clone()],
            ),
            vec![],
        )
    } else {
        tx.move_call(
            Function::new(
                "0x2".parse()?,
                "coin".parse()?,
                "zero".parse()?,
                vec![token_a_tag.clone()],
            ),
            vec![],
        )
    };

    // Step 2: Split coin for input amount.
    debug_bluefin("[bluefin_swap] step 2: split coin for input amount");
    let split_coin = {
        let amt = tx.input(Serialized(&amount_in));
        let split_type = if a2b {
            token_a_tag.clone()
        } else {
            token_b_tag.clone()
        };
        tx.move_call(
            Function::new(
                "0x2".parse()?,
                "coin".parse()?,
                "split".parse()?,
                vec![split_type],
            ),
            vec![token_in, amt],
        )
    };

    // Determine coin_a and coin_b based on direction.
    let (coin_a, coin_b) = if a2b {
        (split_coin, zero_coin)
    } else {
        (zero_coin, split_coin)
    };

    // Step 3: Call swap_assets function.
    debug_bluefin("[bluefin_swap] step 3: calling swap_assets");

    tx.move_call(
        Function::new(
            BLUEFIN_GATEWAY_PACKAGE.parse()?,
            MODULE_NAME.parse()?,
            SWAP_FUNCTION_NAME.parse()?,
            vec![token_a_tag.clone(), token_b_tag.clone()],
        ),
        vec![
            clock,
            global_cfg,
            pool,
            coin_a,
            coin_b,
            a2b_arg,
            by_amount_in_arg,
            amount_arg,
            timestamp_arg,
            sqrt_price_limit_arg,
        ],
    );

    // Step 4: Transfer remaining token back to sender.
    debug_bluefin("[bluefin_swap] step 4: transfer remaining token to sender");
    let sender_arg = tx.input(Serialized(&sender));
    tx.transfer_objects(vec![token_in], sender_arg);

    debug_bluefin("[bluefin_swap] end");
    Ok(())
}

fn debug_bluefin(msg: &str) {
    if DEBUG_BLUEFIN {
        eprintln!("{msg}");
    }
}