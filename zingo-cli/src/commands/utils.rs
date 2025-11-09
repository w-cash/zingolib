//! Module containing utility functions for the commands interface

use json::JsonValue;

use pepper_sync::keys::transparent::TransparentScope;
use zcash_address::ZcashAddress;
use zcash_primitives::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;

use crate::commands::error::CommandError;
use zingolib::data::receivers::Receivers;
use zingolib::utils::conversion::{address_from_str, zatoshis_from_u64};
use zingolib::wallet;
use zingolib::wallet::restrictions::{ShieldedAddressRestriction, TransparentAddressRestriction};

// Parse the send arguments for `do_send`.
// The send arguments have two possible formats:
// - 1 argument in the form of a JSON string for multiple sends. '[{"address":"<address>", "value":<value>, "memo":"<optional memo>"}, ...]'
// - 2 (+1 optional) arguments for a single address send. &["<address>", <amount>, "<optional memo>"]
pub(super) fn parse_send_args(
    args: &[&str],
) -> Result<(Receivers, Option<ShieldedAddressRestriction>), CommandError> {
    if args.len() == 1 {
        let json_args = json::parse(args[0]).map_err(CommandError::ArgsNotJson)?;

        if json_args.is_array() {
            return Ok((parse_receivers_array(&json_args)?, None));
        } else if json_args.is_object() {
            if !json_args.has_key("receivers") {
                return Err(CommandError::MissingKey("receivers".to_string()));
            }
            let receivers_json = &json_args["receivers"];
            if !receivers_json.is_array() {
                return Err(CommandError::UnexpectedType(
                    "\"receivers\" must be an array".to_string(),
                ));
            }
            let receivers = parse_receivers_array(receivers_json)?;
            let restriction = parse_shielded_from(&json_args["from"])?;
            return Ok((receivers, restriction));
        } else {
            return Err(CommandError::SingleArgNotJsonArray(json_args.to_string()));
        }
    }

    if args.len() == 2 || args.len() == 3 {
        let recipient_address =
            address_from_str(args[0]).map_err(CommandError::ConversionFailed)?;
        let amount_u64 = args[1]
            .trim()
            .parse::<u64>()
            .map_err(CommandError::ParseIntFromString)?;
        let amount = zatoshis_from_u64(amount_u64).map_err(CommandError::ConversionFailed)?;
        let memo = if args.len() == 3 {
            Some(
                wallet::utils::interpret_memo_string(args[2].to_string())
                    .map_err(CommandError::InvalidMemo)?,
            )
        } else {
            None
        };
        check_memo_compatibility(&recipient_address, &memo)?;

        return Ok((
            vec![zingolib::data::receivers::Receiver {
                recipient_address,
                amount,
                memo,
            }],
            None,
        ));
    }

    Err(CommandError::InvalidArguments)
}

// The send arguments have two possible formats:
// - 1 arguments in the form of:
//    *  a JSON string (single address only). '[{"address":"<address>", "memo":"<optional memo>", "zennies_for_zingo":<true|false>}]'
// - 1 + 1 optional arguments for a single address send. &["<address>", "<optional memo>"]
fn parse_receivers_array(json_array: &JsonValue) -> Result<Receivers, CommandError> {
    if json_array.is_empty() {
        return Err(CommandError::EmptyJsonArray);
    }

    json_array
        .members()
        .map(|j| {
            let recipient_address = address_from_json(j)?;
            let amount = zatoshis_from_json(j)?;
            let memo = memo_from_json(j)?;
            check_memo_compatibility(&recipient_address, &memo)?;

            Ok(zingolib::data::receivers::Receiver {
                recipient_address,
                amount,
                memo,
            })
        })
        .collect::<Result<Receivers, CommandError>>()
}

pub(super) fn parse_send_all_args(
    args: &[&str],
) -> Result<
    (
        ZcashAddress,
        bool,
        Option<MemoBytes>,
        Option<ShieldedAddressRestriction>,
    ),
    CommandError,
> {
    let address: ZcashAddress;
    let memo: Option<MemoBytes>;
    let zennies_for_zingo: bool;
    let mut restriction = None;
    if args.len() == 1 {
        if let Ok(addr) = address_from_str(args[0]) {
            address = addr;
            memo = None;
            check_memo_compatibility(&address, &memo)?;
            zennies_for_zingo = false;
        } else {
            let json_arg =
                json::parse(args[0]).map_err(|_e| CommandError::ArgNotJsonOrValidAddress)?;
            if json_arg.is_array() {
                return Err(CommandError::JsonArrayNotObj(json_arg.to_string()));
            }
            if json_arg.is_empty() {
                return Err(CommandError::EmptyJsonArray);
            }
            address = address_from_json(&json_arg)?;
            memo = memo_from_json(&json_arg)?;
            check_memo_compatibility(&address, &memo)?;
            zennies_for_zingo = zennies_flag_from_json(&json_arg)?;
            restriction = parse_shielded_from(&json_arg["from"])?;
        }
    } else if args.len() == 2 {
        zennies_for_zingo = false;
        address = address_from_str(args[0]).map_err(CommandError::ConversionFailed)?;
        memo = Some(
            wallet::utils::interpret_memo_string(args[1].to_string())
                .map_err(CommandError::InvalidMemo)?,
        );
        check_memo_compatibility(&address, &memo)?;
    } else {
        return Err(CommandError::InvalidArguments);
    }
    Ok((address, zennies_for_zingo, memo, restriction))
}

// Parse the arguments for `spendable_balance`.
// The arguments have two possible formats:
// - 1 argument in the form of a JSON string (single address only). '[{"address":"<address>", "zennies_for_zingo": <true|false>}]'
// - 1 argument for a single address. &["<address>"]
// NOTE: zennies_for_zingo can only be set in a JSON
// string.
pub(super) fn parse_max_send_value_args(
    args: &[&str],
) -> Result<(ZcashAddress, bool, Option<ShieldedAddressRestriction>), CommandError> {
    if args.len() != 1 {
        return Err(CommandError::InvalidArguments);
    }
    let address: ZcashAddress;
    let zennies_for_zingo: bool;
    let restriction;

    if let Ok(addr) = address_from_str(args[0]) {
        address = addr;
        zennies_for_zingo = false;
        restriction = None;
    } else {
        let json_arg = json::parse(args[0]).map_err(|_e| CommandError::ArgNotJsonOrValidAddress)?;

        if json_arg.is_array() {
            return Err(CommandError::JsonArrayNotObj(
                "Pass an object, not an array.".to_string(),
            ));
        }
        if json_arg.is_empty() {
            return Err(CommandError::EmptyJsonArray);
        }
        address = address_from_json(&json_arg)?;
        zennies_for_zingo = zennies_flag_from_json(&json_arg)?;
        restriction = parse_shielded_from(&json_arg["from"])?;
    }

    Ok((address, zennies_for_zingo, restriction))
}

// Checks send inputs do not contain memo's to transparent addresses.
fn check_memo_compatibility(
    address: &ZcashAddress,
    memo: &Option<MemoBytes>,
) -> Result<(), CommandError> {
    if !address.can_receive_memo() && memo.is_some() {
        return Err(CommandError::IncompatibleMemo);
    }

    Ok(())
}

fn address_from_json(json_array: &JsonValue) -> Result<ZcashAddress, CommandError> {
    if !json_array.has_key("address") {
        return Err(CommandError::MissingKey("address".to_string()));
    }
    let address_str = json_array["address"]
        .as_str()
        .ok_or(CommandError::UnexpectedType(
            "address is not a string!".to_string(),
        ))?;
    address_from_str(address_str).map_err(CommandError::ConversionFailed)
}

fn zennies_flag_from_json(json_arg: &JsonValue) -> Result<bool, CommandError> {
    if !json_arg.has_key("zennies_for_zingo") {
        return Err(CommandError::MissingZenniesForZingoFlag);
    }
    match json_arg["zennies_for_zingo"].as_bool() {
        Some(boolean) => Ok(boolean),
        None => Err(CommandError::ZenniesFlagNonBool(
            json_arg["zennies_for_zingo"].to_string(),
        )),
    }
}

fn zatoshis_from_json(json_array: &JsonValue) -> Result<Zatoshis, CommandError> {
    if !json_array.has_key("amount") {
        return Err(CommandError::MissingKey("amount".to_string()));
    }
    let amount_u64 = if json_array["amount"].is_number() {
        json_array["amount"]
            .as_u64()
            .ok_or(CommandError::UnexpectedType(
                "amount not a u64!".to_string(),
            ))?
    } else {
        return Err(CommandError::NonJsonNumberForAmount(format!(
            "\"amount\": {}\nis not a json::number::Number",
            json_array["amount"]
        )));
    };
    zatoshis_from_u64(amount_u64).map_err(CommandError::ConversionFailed)
}

fn memo_from_json(json_array: &JsonValue) -> Result<Option<MemoBytes>, CommandError> {
    if let Some(m) = json_array["memo"]
        .as_str()
        .map(std::string::ToString::to_string)
    {
        let memo = wallet::utils::interpret_memo_string(m).map_err(CommandError::InvalidMemo)?;
        Ok(Some(memo))
    } else {
        Ok(None)
    }
}

fn parse_shielded_from(
    value: &JsonValue,
) -> Result<Option<ShieldedAddressRestriction>, CommandError> {
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_object() {
        return Err(CommandError::UnexpectedType(
            "\"from\" must be a JSON object".to_string(),
        ));
    }
    let scope = parse_shielded_scope(value["scope"].as_str())?;
    if scope != zip32::Scope::External {
        return Err(CommandError::UnexpectedType(
            "shielded restrictions currently support external scope only".to_string(),
        ));
    }
    let account_id = parse_account_id(value["account"].as_u64())?;
    let start_index = parse_required_u32(value, "address_index")?;
    let max_index = parse_optional_u32(value, "max_address_index")?;
    if let Some(max_index) = max_index {
        if max_index < start_index {
            return Err(CommandError::UnexpectedType(
                "\"max_address_index\" must be >= \"address_index\"".to_string(),
            ));
        }
    }

    Ok(Some(ShieldedAddressRestriction::new(
        account_id,
        start_index,
        max_index,
    )))
}

fn parse_transparent_from(
    value: &JsonValue,
) -> Result<Option<TransparentAddressRestriction>, CommandError> {
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_object() {
        return Err(CommandError::UnexpectedType(
            "transparent selector must be a JSON object".to_string(),
        ));
    }
    let scope = parse_transparent_scope(value["scope"].as_str())?;
    let account_id = parse_account_id(value["account"].as_u64())?;
    let start_index = parse_required_u32(value, "address_index")?;
    let max_index = parse_optional_u32(value, "max_address_index")?;
    if let Some(max_index) = max_index {
        if max_index < start_index {
            return Err(CommandError::UnexpectedType(
                "\"max_address_index\" must be >= \"address_index\"".to_string(),
            ));
        }
    }

    Ok(Some(TransparentAddressRestriction::new(
        account_id,
        scope,
        start_index,
        max_index,
    )))
}

fn parse_required_u32(target: &JsonValue, key: &str) -> Result<u32, CommandError> {
    if !target.has_key(key) {
        return Err(CommandError::MissingKey(key.to_string()));
    }
    target[key].as_u64().map(|v| v as u32).ok_or_else(|| {
        CommandError::UnexpectedType(format!("\"{key}\" must be a non-negative integer"))
    })
}

fn parse_optional_u32(target: &JsonValue, key: &str) -> Result<Option<u32>, CommandError> {
    if !target.has_key(key) || target[key].is_null() {
        return Ok(None);
    }
    target[key]
        .as_u64()
        .map(|v| v as u32)
        .ok_or_else(|| {
            CommandError::UnexpectedType(format!("\"{key}\" must be a non-negative integer"))
        })
        .map(Some)
}

fn extract_restriction_object<'a>(json_arg: &'a JsonValue) -> Result<&'a JsonValue, CommandError> {
    if !json_arg.is_object() {
        return Err(CommandError::UnexpectedType(
            "expected a JSON object describing the selector".to_string(),
        ));
    }
    if json_arg.has_key("from") {
        let from = &json_arg["from"];
        if !from.is_object() || from.is_null() {
            return Err(CommandError::UnexpectedType(
                "\"from\" must be a JSON object".to_string(),
            ));
        }
        Ok(from)
    } else {
        Ok(json_arg)
    }
}

fn parse_account_id(raw: Option<u64>) -> Result<zip32::AccountId, CommandError> {
    let value = raw.unwrap_or(0);
    zip32::AccountId::try_from(value as u32)
        .map_err(|_| CommandError::UnexpectedType("account id is out of range".to_string()))
}

fn parse_shielded_scope(value: Option<&str>) -> Result<zip32::Scope, CommandError> {
    match value.unwrap_or("external").to_lowercase().as_str() {
        "external" => Ok(zip32::Scope::External),
        "internal" => Ok(zip32::Scope::Internal),
        other => Err(CommandError::UnexpectedType(format!(
            "invalid scope \"{other}\""
        ))),
    }
}

fn parse_transparent_scope(value: Option<&str>) -> Result<TransparentScope, CommandError> {
    match value.unwrap_or("external").to_lowercase().as_str() {
        "external" => Ok(TransparentScope::External),
        "internal" => Ok(TransparentScope::Internal),
        "refund" => Ok(TransparentScope::Refund),
        other => Err(CommandError::UnexpectedType(format!(
            "invalid scope \"{other}\""
        ))),
    }
}

pub(super) fn parse_transparent_restriction_args(
    args: &[&str],
) -> Result<Option<TransparentAddressRestriction>, CommandError> {
    if args.is_empty() {
        return Ok(None);
    }
    if args.len() == 1 {
        let json_arg = json::parse(args[0]).map_err(CommandError::ArgsNotJson)?;
        let restriction_json = extract_restriction_object(&json_arg)?;
        return parse_transparent_from(restriction_json);
    }
    Err(CommandError::InvalidArguments)
}

#[cfg(test)]
mod tests {
    use zingolib::{
        data::receivers::Receiver,
        utils::conversion::{address_from_str, zatoshis_from_u64},
        wallet::{self, utils::interpret_memo_string},
    };

    use crate::commands::error::CommandError;

    #[test]
    fn parse_send_args() {
        let address_str = "zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p";
        let recipient_address = address_from_str(address_str).unwrap();
        let value_str = "100000";
        let amount = zatoshis_from_u64(100_000).unwrap();
        let memo_str = "test memo";
        let memo = wallet::utils::interpret_memo_string(memo_str.to_string()).unwrap();

        // No memo
        let send_args = &[address_str, value_str];
        assert_eq!(
            super::parse_send_args(send_args).unwrap(),
            vec![zingolib::data::receivers::Receiver {
                recipient_address: recipient_address.clone(),
                amount,
                memo: None
            }]
        );

        // Memo
        let send_args = &[address_str, value_str, memo_str];
        assert_eq!(
            super::parse_send_args(send_args).unwrap(),
            vec![Receiver {
                recipient_address: recipient_address.clone(),
                amount,
                memo: Some(memo.clone())
            }]
        );

        // Json
        let json = "[{\"address\":\"tmBsTi2xWTjUdEXnuTceL7fecEQKeWaPDJd\", \"amount\":50000}, \
                    {\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"amount\":100000, \"memo\":\"test memo\"}]";
        assert_eq!(
            super::parse_send_args(&[json]).unwrap(),
            vec![
                Receiver {
                    recipient_address: address_from_str("tmBsTi2xWTjUdEXnuTceL7fecEQKeWaPDJd")
                        .unwrap(),
                    amount: zatoshis_from_u64(50_000).unwrap(),
                    memo: None
                },
                Receiver {
                    recipient_address: recipient_address.clone(),
                    amount,
                    memo: Some(memo.clone())
                }
            ]
        );

        // Trim whitespace
        let send_args = &[address_str, "1 ", memo_str];
        assert_eq!(
            super::parse_send_args(send_args).unwrap(),
            vec![Receiver {
                recipient_address,
                amount: zatoshis_from_u64(1).unwrap(),
                memo: Some(memo.clone())
            }]
        );
    }

    mod fail_parse_send_args {
        use crate::commands::{error::CommandError, utils::parse_send_args};

        mod json_array {
            use super::*;

            #[test]
            fn empty_json_array() {
                let json = "[]";
                assert!(matches!(
                    parse_send_args(&[json]),
                    Err(CommandError::EmptyJsonArray)
                ));
            }
            #[test]
            fn failed_json_parsing() {
                let args = [r"testaddress{{"];
                assert!(matches!(
                    parse_send_args(&args),
                    Err(CommandError::ArgsNotJson(_))
                ));
            }
            #[test]
            fn single_arg_not_an_array_unexpected_type() {
                let args = ["1"];
                assert!(matches!(
                    parse_send_args(&args),
                    Err(CommandError::SingleArgNotJsonArray(_))
                ));
            }
            #[test]
            fn invalid_memo() {
                let arg_contents = "[{\"address\": \"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \"amount\": 123, \"memo\": \"testmemo\"}]";
                let long_513_byte_memo = &"a".repeat(513);
                let long_memo_args =
                    arg_contents.replace("\"testmemo\"", &format!("\"{long_513_byte_memo}\""));
                let args = [long_memo_args.as_str()];

                assert!(matches!(
                    parse_send_args(&args),
                    Err(CommandError::InvalidMemo(_))
                ));
            }
        }
        mod multi_string_args {
            use super::*;

            #[test]
            fn two_args_wrong_amount() {
                let args = [
                    "zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p",
                    "foo",
                ];
                assert!(matches!(
                    parse_send_args(&args),
                    Err(CommandError::ParseIntFromString(_))
                ));
            }
            #[test]
            fn wrong_number_of_args() {
                let args = [
                    "zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p",
                    "123",
                    "3",
                    "4",
                ];
                assert!(matches!(
                    parse_send_args(&args),
                    Err(CommandError::InvalidArguments)
                ));
            }
            #[test]
            fn invalid_memo() {
                let long_513_byte_memo = &"a".repeat(513);
                let args = [
                    "zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p",
                    "123",
                    long_513_byte_memo,
                ];

                assert!(matches!(
                    parse_send_args(&args),
                    Err(CommandError::InvalidMemo(_))
                ));
            }
        }
    }

    #[test]
    fn parse_send_all_args() {
        let address_str = "zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p";
        let address = address_from_str(address_str).unwrap();
        let memo_str = "test memo";
        let memo = wallet::utils::interpret_memo_string(memo_str.to_string()).unwrap();

        // JSON single receiver
        let single_receiver = &[
            "{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                 \"memo\":\"test memo\", \
                 \"zennies_for_zingo\":false}",
        ];
        assert_eq!(
            super::parse_send_all_args(single_receiver).unwrap(),
            (address.clone(), false, Some(memo.clone()))
        );
        // NonBool Zenny Flag
        let nb_zenny = &[
            "{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                 \"memo\":\"test memo\", \
                 \"zennies_for_zingo\":\"false\"}",
        ];
        assert!(matches!(
            super::parse_send_all_args(nb_zenny),
            Err(CommandError::ZenniesFlagNonBool(_))
        ));
        // with memo
        let send_args = &[address_str, memo_str];
        assert_eq!(
            super::parse_send_all_args(send_args).unwrap(),
            (address.clone(), false, Some(memo.clone()))
        );
        let send_args = &[address_str, memo_str];
        assert_eq!(
            super::parse_send_all_args(send_args).unwrap(),
            (address.clone(), false, Some(memo.clone()))
        );

        // invalid address
        let send_args = &["invalid_address"];
        assert!(matches!(
            super::parse_send_all_args(send_args),
            Err(CommandError::ArgNotJsonOrValidAddress)
        ));
    }

    #[test]
    fn check_memo_compatibility() {
        let sapling_address = address_from_str("zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p").unwrap();
        let transparent_address = address_from_str("tmBsTi2xWTjUdEXnuTceL7fecEQKeWaPDJd").unwrap();
        let memo = interpret_memo_string("test memo".to_string()).unwrap();

        // shielded address with memo
        super::check_memo_compatibility(&sapling_address, &Some(memo.clone())).unwrap();

        // transparent address without memo
        super::check_memo_compatibility(&transparent_address, &None).unwrap();

        // transparent address with memo
        assert!(matches!(
            super::check_memo_compatibility(&transparent_address, &Some(memo.clone())),
            Err(CommandError::IncompatibleMemo)
        ));
    }

    #[test]
    fn address_from_json() {
        // with address
        let json_str = "[{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"amount\":100000, \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        super::address_from_json(json_args).unwrap();

        // without address
        let json_str = "[{\"amount\":100000, \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        assert!(matches!(
            super::address_from_json(json_args),
            Err(CommandError::MissingKey(_))
        ));

        // invalid address
        let json_str = "[{\"address\": 1, \
                    \"amount\":100000, \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        assert!(matches!(
            super::address_from_json(json_args),
            Err(CommandError::UnexpectedType(_))
        ));
    }

    #[test]
    fn zatoshis_from_json() {
        // with amount
        let json_str = "[{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"amount\":100000, \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        super::zatoshis_from_json(json_args).unwrap();

        // without amount
        let json_str = "[{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        assert!(matches!(
            super::zatoshis_from_json(json_args),
            Err(CommandError::MissingKey(_))
        ));

        // invalid amount
        let json_str = "[{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"amount\":\"non_number\", \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        assert!(matches!(
            super::zatoshis_from_json(json_args),
            Err(CommandError::NonJsonNumberForAmount(_))
        ));
    }

    #[test]
    fn memo_from_json() {
        // with memo
        let json_str = "[{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"amount\":100000, \"memo\":\"test memo\"}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        assert_eq!(
            super::memo_from_json(json_args).unwrap(),
            Some(interpret_memo_string("test memo".to_string()).unwrap())
        );

        // without memo
        let json_str = "[{\"address\":\"zregtestsapling1fmq2ufux3gm0v8qf7x585wj56le4wjfsqsj27zprjghntrerntggg507hxh2ydcdkn7sx8kya7p\", \
                    \"amount\":100000}]";
        let json_args = json::parse(json_str).unwrap();
        let json_args = json_args.members().next().unwrap();
        assert_eq!(super::memo_from_json(json_args).unwrap(), None);
    }
}
