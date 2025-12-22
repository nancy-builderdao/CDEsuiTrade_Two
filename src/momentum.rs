use std::error::Error;

use sui_sdk_types::{Address, Argument, TypeTag};
use sui_transaction_builder::unresolved::Input;
use sui_transaction_builder::{Function, Serialized, TransactionBuilder};

/// Enable / disable debug logs inside momentum module.
const DEBUG_MOMENTUM: bool = true;

/// Move package addresses and type tags.
const MOMENTUM_TRADE_PACKAGE: &str =
    "0xcf60a40f45d46fc1e828871a647c1e25a0915dec860d2662eb10fdb382c3c1d1";
const MOMENTUM_SLIPPAGE_PACKAGE: &str =
    "0x8add2f0f8bc9748687639d7eb59b2172ba09a0172d9e63c029e23a7dbdb6abe6";

const A_TOKEN_TYPE: &str =
    "0x2::sui::SUI";
const B_TOKEN_TYPE: &str =
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

/// Global object ids used inside the transaction (clock, versioned object, etc.).


/// Build a swap transaction using only Input::by_id for all object inputs.
///
/// - `token_input`: input for the token coin.
/// - `pool_input`: input for the pool shared object.
/// - `gas_object_id`: object id of the SUI gas coin.
/// - `amount`: input token amount.
/// - `direction`: true for A -> B, false for B -> A.
/// - `sender`: transaction sender.
/// - `gas_budget`, `gas_price`: gas configuration.
pub fn create_swap_transaction(
    token_input: Input,
    pool_input: Input,
    gas_input: Input,
    amount: u64,
    direction: bool,
    sender: Address,
    gas_budget: u64,
    gas_price: u64,
    clock_input: Input,
    version_input: Input,
) -> Result<sui_sdk_types::Transaction, Box<dyn Error>> {
    debug_momentum("[create_swap_transaction] start");

    let mut tx = TransactionBuilder::new();

    // Transaction metadata.
    tx.set_sender(sender);
    tx.set_gas_budget(gas_budget);
    tx.set_gas_price(gas_price);

    debug_momentum(&format!(
        "[create_swap_transaction] sender={sender}, gas_budget={gas_budget}, gas_price={gas_price}"
    ));

    // Gas input via by_id.
    debug_momentum(&format!(
        "[create_swap_transaction] adding gas object by_id: {gas_input:?}"
    ));
    println!("{gas_input:?}");
    tx.add_gas_objects(vec![gas_input]);

    // Token input and pool input via by_id.
    debug_momentum("[create_swap_transaction] adding token and pool inputs");
    let token_input = tx.input(token_input);
    let pool_input = tx.input(pool_input);
    let clock_input = tx.input(clock_input);
    let version_input = tx.input(version_input);
    // Build swap logic.
    debug_momentum("[create_swap_transaction] before swap()");
    swap(&mut tx, token_input, amount, direction, pool_input, sender, clock_input, version_input)?;
    debug_momentum("[create_swap_transaction] after swap()");

    // Finalize transaction.
    debug_momentum("[create_swap_transaction] before finish()");
    let transaction = tx.finish()?;
    debug_momentum("[create_swap_transaction] after finish()");

    Ok(transaction)
}

/// Core swap logic built on top of TransactionBuilder.
///
/// All objects are added to the builder using Input::by_id or Serialized.
pub fn swap(
    tx: &mut TransactionBuilder,
    token_input: Argument,
    amount: u64,
    direction: bool,
    pool: Argument,
    sender: Address,
    clock_object: Argument,
    versioned_object: Argument,
) -> Result<(), Box<dyn Error>> {
    debug_momentum(&format!(
        "[swap] start, amount={amount}, direction={direction}, sender={sender}"
    ));

    let a_token_type: TypeTag = A_TOKEN_TYPE.parse()?;
    let b_token_type: TypeTag = B_TOKEN_TYPE.parse()?;

    // Clock object (global).
    debug_momentum("[swap] adding clock object");
    // let clock_object = tx.input(Input::by_id(CLOCK_OBJECT_ID.parse()?).with_shared_kind());

    // Versioned object (global configuration for the pool).
    debug_momentum("[swap] adding versioned object");
    // 1. Split token coins (create an internal split for repayment later).
    debug_momentum("[swap] step 1: split_coins");
    let amount_arg = tx.input(Serialized(&amount));
    let split_coin = tx.split_coins(token_input, vec![amount_arg]);

    // 2. Flash swap.
    let sqrt_price_limit: u128 = if direction {
        // A -> B
        4_295_048_016u128
    } else {
        // B -> A
        79_226_673_515_401_279_992_447_579_055u128
    };

    debug_momentum(&format!(
        "[swap] step 2: flash_swap, sqrt_price_limit={sqrt_price_limit}"
    ));

    let direction_arg = tx.input(Serialized(&direction));
    let by_amount_in_arg = tx.input(Serialized(&true));
    let amount_arg_2 = tx.input(Serialized(&amount));
    let sqrt_price_limit_arg = tx.input(Serialized(&sqrt_price_limit));

    let flash_swap_result = tx.move_call(
        Function::new(
            MOMENTUM_TRADE_PACKAGE.parse()?,
            "trade".parse()?,
            "flash_swap".parse()?,
            vec![a_token_type.clone(), b_token_type.clone()],
        ),
        vec![
            pool,
            direction_arg,
            by_amount_in_arg,
            amount_arg_2,
            sqrt_price_limit_arg,
            clock_object,
            versioned_object,
        ],
    );

    debug_momentum("[swap] after flash_swap");

    // flash_swap returns: balance_a, balance_b, receipt.
    let flash_swap_r1 = flash_swap_result
        .nested(0)
        .expect("flash_swap result index 0 missing");
    let flash_swap_r2 = flash_swap_result
        .nested(1)
        .expect("flash_swap result index 1 missing");
    let flash_swap_r3 = flash_swap_result
        .nested(2)
        .expect("flash_swap result index 2 missing");

    // 3. Destroy the zero balance (input side after swap).
    debug_momentum("[swap] step 3: destroy_zero & select output_balance");
    let (zero_balance, output_balance) = if direction {
        (flash_swap_r1, flash_swap_r2)
    } else {
        (flash_swap_r2, flash_swap_r1)
    };

    let zero_type = if direction {
        a_token_type.clone()
    } else {
        b_token_type.clone()
    };
    let output_type = if direction {
        b_token_type.clone()
    } else {
        a_token_type.clone()
    };

    tx.move_call(
        Function::new(
            "0x2".parse()?,
            "balance".parse()?,
            "destroy_zero".parse()?,
            vec![zero_type],
        ),
        vec![zero_balance],
    );

    // 4. Convert output balance to coin.
    debug_momentum("[swap] step 4: from_balance -> output_coin");
    let output_coin = tx.move_call(
        Function::new(
            "0x2".parse()?,
            "coin".parse()?,
            "from_balance".parse()?,
            vec![output_type.clone()],
        ),
        vec![output_balance],
    );

    // 5. Get receipt debts.
    debug_momentum("[swap] step 5: swap_receipt_debts");
    let receipt_debts_result = tx.move_call(
        Function::new(
            MOMENTUM_TRADE_PACKAGE.parse()?,
            "trade".parse()?,
            "swap_receipt_debts".parse()?,
            vec![],
        ),
        vec![flash_swap_r3],
    );

    let receipt_debt_a = receipt_debts_result
        .nested(0)
        .expect("swap_receipt_debts result index 0 missing");
    let receipt_debt_b = receipt_debts_result
        .nested(1)
        .expect("swap_receipt_debts result index 1 missing");

    let repay_debt = if direction {
        receipt_debt_a
    } else {
        receipt_debt_b
    };

    let repay_type = if direction {
        a_token_type.clone()
    } else {
        b_token_type.clone()
    };
    let zero_coin_type = if direction {
        b_token_type.clone()
    } else {
        a_token_type.clone()
    };

    // 6. Split coin for repayment.
    debug_momentum("[swap] step 6: coin::split for repayment");
    let repay_from_split = tx.move_call(
        Function::new(
            "0x2".parse()?,
            "coin".parse()?,
            "split".parse()?,
            vec![repay_type.clone()],
        ),
        vec![split_coin, repay_debt],
    );

    // 7. Convert repay coin to balance.
    debug_momentum("[swap] step 7: coin::into_balance for repay");
    let balance1 = tx.move_call(
        Function::new(
            "0x2".parse()?,
            "coin".parse()?,
            "into_balance".parse()?,
            vec![repay_type.clone()],
        ),
        vec![repay_from_split],
    );

    // 8. Create zero coin.
    debug_momentum("[swap] step 8: coin::zero");
    let zero_coin = tx.move_call(
        Function::new(
            "0x2".parse()?,
            "coin".parse()?,
            "zero".parse()?,
            vec![zero_coin_type.clone()],
        ),
        vec![],
    );

    // 9. Convert zero coin to balance.
    debug_momentum("[swap] step 9: coin::into_balance for zero_coin");
    let balance2 = tx.move_call(
        Function::new(
            "0x2".parse()?,
            "coin".parse()?,
            "into_balance".parse()?,
            vec![zero_coin_type],
        ),
        vec![zero_coin],
    );

    // 10. Repay flash swap.
    debug_momentum("[swap] step 10: repay_flash_swap");
    let (repay_balance_a, repay_balance_b) = if direction {
        (balance1, balance2)
    } else {
        (balance2, balance1)
    };

    tx.move_call(
        Function::new(
            MOMENTUM_TRADE_PACKAGE.parse()?,
            "trade".parse()?,
            "repay_flash_swap".parse()?,
            vec![a_token_type.clone(), b_token_type.clone()],
        ),
        vec![pool, flash_swap_r3, repay_balance_a, repay_balance_b, versioned_object],
    );

    // 11. Slippage check.
    let slippage_limit: u128 = if direction {
        0u128
    } else {
        u64::MAX as u128
    };

    debug_momentum(&format!(
        "[swap] step 11: assert_slippage, slippage_limit={slippage_limit}"
    ));

    let slippage_limit_arg = tx.input(Serialized(&slippage_limit));
    let direction_arg_2 = tx.input(Serialized(&direction));

    tx.move_call(
        Function::new(
            MOMENTUM_SLIPPAGE_PACKAGE.parse()?,
            "slippage_check".parse()?,
            "assert_slippage".parse()?,
            vec![a_token_type, b_token_type],
        ),
        vec![pool, slippage_limit_arg, direction_arg_2],
    );

    // 12. Transfer output coin + remaining split coin back to sender.
    debug_momentum("[swap] step 12: transfer_objects back to sender");
    let sender_arg = tx.input(Serialized(&sender));
    tx.transfer_objects(vec![output_coin, split_coin], sender_arg);

    debug_momentum("[swap] end");
    Ok(())
}

fn debug_momentum(msg: &str) {
    if DEBUG_MOMENTUM {
        eprintln!("{msg}");
    }
}