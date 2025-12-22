use std::error::Error;

use sui_sdk_types::{Address, Argument, TypeTag};
use sui_transaction_builder::{Function, Serialized, TransactionBuilder};
use sui_transaction_builder::unresolved::Input;

/// Enable / disable debug logs inside cetus module.
const DEBUG_CETUS: bool = true;

// ====== Cetus constants ======
const CETUS_SWAP_PACKAGE: &str =
    "0xb2db7142fa83210a7d78d9c12ac49c043b3cbbd482224fea6e3da00aa5a5ae2d";
const MODULE_NAME: &str = "pool_script_v2";
const SWAP_B2A_FUNCTION_NAME: &str = "swap_b2a";
const SWAP_A2B_FUNCTION_NAME: &str = "swap_a2b";

// price limits
const MAX_SQRT_PRICE: u128 = 79_226_673_515_401_279_992_447_579_055u128;
const MIN_SQRT_PRICE: u128 = 4_295_048_016u128;

/// Build a Cetus swap transaction using Input objects (same pattern as momentum.rs).
///
/// - `token_input`: input for the token coin (owned object).
/// - `pool_input`: input for the pool shared object.
/// - `global_config_input`: input for the global config shared object.
/// - `clock_input`: input for the clock shared object.
/// - `gas_input`: input for the gas coin object.
/// - `amount_in`: input token amount.
/// - `min_amount_out`: minimum output amount (slippage protection).
/// - `a2b`: true for A -> B, false for B -> A.
/// - `sender`: transaction sender.
/// - `gas_budget`, `gas_price`: gas configuration.
/// - `token_a_type`, `token_b_type`: token type strings.
pub fn create_cetus_swap_transaction(
    token_input: Input,
    pool_input: Input,
    global_config_input: Input,
    clock_input: Input,
    gas_input: Input,
    amount_in: u64,
    min_amount_out: u64,
    a2b: bool,
    sender: Address,
    gas_budget: u64,
    gas_price: u64,
    token_a_type: &str,
    token_b_type: &str,
) -> Result<sui_sdk_types::Transaction, Box<dyn Error>> {
    debug_cetus("[create_cetus_swap_transaction] start");

    let mut tx = TransactionBuilder::new();

    // Transaction metadata.
    tx.set_sender(sender);
    tx.set_gas_budget(gas_budget);
    tx.set_gas_price(gas_price);

    debug_cetus(&format!(
        "[create_cetus_swap_transaction] sender={sender}, gas_budget={gas_budget}, gas_price={gas_price}"
    ));

    // Gas input.
    debug_cetus(&format!(
        "[create_cetus_swap_transaction] adding gas object: {gas_input:?}"
    ));
    tx.add_gas_objects(vec![gas_input]);

    // Add inputs to transaction.
    debug_cetus("[create_cetus_swap_transaction] adding token, pool, global_config, clock inputs");
    let token_in = tx.input(token_input);
    let global_cfg = tx.input(global_config_input);
    let pool = tx.input(pool_input);
    let clock = tx.input(clock_input);

    // Build swap logic.
    debug_cetus("[create_cetus_swap_transaction] before cetus_swap()");
    cetus_swap(
        &mut tx,
        token_in,
        pool,
        global_cfg,
        clock,
        amount_in,
        min_amount_out,
        a2b,
        token_a_type,
        token_b_type,
        sender,
    )?;
    debug_cetus("[create_cetus_swap_transaction] after cetus_swap()");

    // Finalize transaction.
    debug_cetus("[create_cetus_swap_transaction] before finish()");
    let transaction = tx.finish()?;
    debug_cetus("[create_cetus_swap_transaction] after finish()");

    Ok(transaction)
}

/// Core Cetus swap logic built on top of TransactionBuilder.
fn cetus_swap(
    tx: &mut TransactionBuilder,
    token_in: Argument,
    pool: Argument,
    global_cfg: Argument,
    clock: Argument,
    amount_in: u64,
    min_amount_out: u64,
    a2b: bool,
    token_a_type: &str,
    token_b_type: &str,
    sender: Address,
) -> Result<(), Box<dyn Error>> {
    debug_cetus(&format!(
        "[cetus_swap] start, amount_in={amount_in}, min_amount_out={min_amount_out}, a2b={a2b}"
    ));

    let token_a_tag: TypeTag = token_a_type.parse()?;
    let token_b_tag: TypeTag = token_b_type.parse()?;

    // Pure args.
    let by_amount_in_arg = tx.input(Serialized(&true));
    let amount_in_arg = tx.input(Serialized(&amount_in));
    let min_amount_out_arg = tx.input(Serialized(&min_amount_out));

    let sqrt_price_limit: u128 = if !a2b { MIN_SQRT_PRICE } else { MAX_SQRT_PRICE };
    let sqrt_price_limit_arg = tx.input(Serialized(&sqrt_price_limit));

    debug_cetus(&format!(
        "[cetus_swap] sqrt_price_limit={sqrt_price_limit}"
    ));

    // Step 1: Create zero coin for output side.
    debug_cetus("[cetus_swap] step 1: create zero coin for output");
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
    debug_cetus("[cetus_swap] step 2: split coin for input amount");
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
        (zero_coin, split_coin)
    } else {
        (split_coin, zero_coin)
    };

    // Step 3: Call swap function.
    let fn_name = if !a2b {
        SWAP_A2B_FUNCTION_NAME
    } else {
        SWAP_B2A_FUNCTION_NAME
    };

    debug_cetus(&format!("[cetus_swap] step 3: calling {fn_name}"));

    tx.move_call(
        Function::new(
            CETUS_SWAP_PACKAGE.parse()?,
            MODULE_NAME.parse()?,
            fn_name.parse()?,
            vec![token_b_tag.clone(), token_a_tag.clone()],
        ),
        vec![
            global_cfg,
            pool,
            coin_a,
            coin_b,
            by_amount_in_arg,
            amount_in_arg,
            min_amount_out_arg,
            sqrt_price_limit_arg,
            clock,
        ],
    );

    // Step 4: Transfer remaining token back to sender.
    debug_cetus("[cetus_swap] step 4: transfer remaining token to sender");
    let sender_arg = tx.input(Serialized(&sender));
    tx.transfer_objects(vec![token_in], sender_arg);

    debug_cetus("[cetus_swap] end");
    Ok(())
}

fn debug_cetus(msg: &str) {
    if DEBUG_CETUS {
        eprintln!("{msg}");
    }
}