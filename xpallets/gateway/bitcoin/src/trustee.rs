use frame_support::{
    debug::native,
    dispatch::{DispatchError, DispatchResult},
    StorageValue,
};
use sp_runtime::SaturatedConversion;
use sp_std::{convert::TryFrom, prelude::*, result};

use btc_chain::Transaction;
use btc_crypto::dhash160;
use btc_keys::{Address, Public, Type};
use btc_primitives::Bytes;
use btc_script::{Builder, Opcode, Script};

use xpallet_assets::Chain;
use xpallet_gateway_common::{
    traits::{TrusteeForChain, TrusteeSession},
    trustees::bitcoin::{BtcTrusteeAddrInfo, BtcTrusteeType},
    types::{TrusteeInfoConfig, TrusteeIntentionProps, TrusteeSessionInfo},
    utils::two_thirds_unsafe,
};
use xpallet_support::{debug, error, info, RUNTIME_TARGET};

use crate::tx::utils::{addr2vecu8, ensure_identical, parse_output_addr};
use crate::tx::validator::parse_and_check_signed_tx;
use crate::types::{BtcWithdrawalProposal, VoteResult};
use crate::{Error, Module, RawEvent, Trait, WithdrawalProposal};

pub fn trustee_session<T: Trait>(
) -> Result<TrusteeSessionInfo<T::AccountId, BtcTrusteeAddrInfo>, DispatchError> {
    T::TrusteeSessionProvider::current_trustee_session()
}

#[inline]
fn trustee_addr_info_pair<T: Trait>(
) -> Result<(BtcTrusteeAddrInfo, BtcTrusteeAddrInfo), DispatchError> {
    T::TrusteeSessionProvider::current_trustee_session()
        .map(|session_info| (session_info.hot_address, session_info.cold_address))
}

#[inline]
pub fn get_trustee_address_pair<T: Trait>() -> Result<(Address, Address), DispatchError> {
    trustee_addr_info_pair::<T>().map(|(hot_info, cold_info)| {
        (
            Module::<T>::verify_btc_address(&hot_info.addr)
                .expect("should not parse error from storage data; qed"),
            Module::<T>::verify_btc_address(&cold_info.addr)
                .expect("should not parse error from storage data; qed"),
        )
    })
}

#[inline]
pub fn get_last_trustee_address_pair<T: Trait>() -> Result<(Address, Address), DispatchError> {
    T::TrusteeSessionProvider::last_trustee_session().map(|session_info| {
        (
            Module::<T>::verify_btc_address(&session_info.hot_address.addr)
                .expect("should not parse error from storage data; qed"),
            Module::<T>::verify_btc_address(&session_info.cold_address.addr)
                .expect("should not parse error from storage data; qed"),
        )
    })
}

pub fn get_hot_trustee_address<T: Trait>() -> Result<Address, DispatchError> {
    trustee_addr_info_pair::<T>()
        .and_then(|(addr_info, _)| Module::<T>::verify_btc_address(&addr_info.addr))
}

pub fn get_hot_trustee_redeem_script<T: Trait>() -> Result<Script, DispatchError> {
    trustee_addr_info_pair::<T>().map(|(addr_info, _)| addr_info.redeem_script.into())
}

fn check_keys<T: Trait>(keys: &[Public]) -> DispatchResult {
    let has_duplicate = (1..keys.len()).any(|i| keys[i..].contains(&keys[i - 1]));
    if has_duplicate {
        error!("[generate_new_trustees]|keys contains duplicate pubkey");
        Err(Error::<T>::DuplicatedKeys)?;
    }
    if keys.iter().any(|public: &Public| {
        if let Public::Normal(_) = public {
            true
        } else {
            false
        }
    }) {
        Err("unexpect! all keys(bitcoin Public) should be compressed")?;
    }
    Ok(())
}

//const EC_P = Buffer.from('fffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2f', 'hex')
const EC_P: [u8; 32] = [
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 254, 255, 255, 252, 47,
];

const ZERO_P: [u8; 32] = [0; 32];

impl<T: Trait> TrusteeForChain<T::AccountId, BtcTrusteeType, BtcTrusteeAddrInfo> for Module<T> {
    fn check_trustee_entity(raw_addr: &[u8]) -> result::Result<BtcTrusteeType, DispatchError> {
        let trustee_type = BtcTrusteeType::try_from(raw_addr.to_vec())
            .map_err(|_| Error::<T>::InvalidPublicKey)?;
        let public = trustee_type.0;
        if let Public::Normal(_) = public {
            error!("not allow Normal Public for bitcoin now");
            Err(Error::<T>::InvalidPublicKey)?
        }

        if 2 != raw_addr[0] && 3 != raw_addr[0] {
            error!("not Compressed Public(prefix not 2|3)");
            Err(Error::<T>::InvalidPublicKey)?
        }

        if &ZERO_P == &raw_addr[1..33] {
            error!("not Compressed Public(Zero32)");
            Err(Error::<T>::InvalidPublicKey)?
        }

        if &raw_addr[1..33] >= &EC_P {
            error!("not Compressed Public(EC_P)");
            Err(Error::<T>::InvalidPublicKey)?
        }

        Ok(BtcTrusteeType(public))
    }

    fn generate_trustee_session_info(
        props: Vec<(T::AccountId, TrusteeIntentionProps<BtcTrusteeType>)>,
        config: TrusteeInfoConfig,
    ) -> result::Result<TrusteeSessionInfo<T::AccountId, BtcTrusteeAddrInfo>, DispatchError> {
        // judge all props has different pubkey
        // check
        let (trustees, props_info): (
            Vec<T::AccountId>,
            Vec<TrusteeIntentionProps<BtcTrusteeType>>,
        ) = props.into_iter().unzip();

        let (hot_keys, cold_keys): (Vec<Public>, Vec<Public>) = props_info
            .into_iter()
            .map(|props| (props.hot_entity.0, props.cold_entity.0))
            .unzip();

        check_keys::<T>(&hot_keys)?;
        check_keys::<T>(&cold_keys)?;

        // [min, max] e.g. bitcoin min is 4, max is 15
        if (trustees.len() as u32) < config.min_trustee_count
            || (trustees.len() as u32) > config.max_trustee_count
        {
            error!("[generate_trustee_session_info]|trustees is less/more than {{min:[{:}], max:[{:}]}} people, can't generate trustee addr|trustees:{:?}",
                   config.min_trustee_count, config.max_trustee_count, trustees);
            Err(Error::<T>::InvalidTrusteeCounts)?;
        }

        #[cfg(feature = "std")]
        let pretty_print_keys = |keys: &[Public]| {
            keys.iter()
                .map(|k| k.to_string().replace("\n", ""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        #[cfg(feature = "std")]
        info!(
            "[generate_trustee_session_info]|hot_keys:[{}]|cold_keys:[{}]",
            pretty_print_keys(&hot_keys),
            pretty_print_keys(&cold_keys)
        );

        #[cfg(not(feature = "std"))]
        info!(
            "[generate_trustee_session_info]|hot_keys:{:?}|cold_keys:{:?}",
            hot_keys, cold_keys
        );

        let sig_num = two_thirds_unsafe(trustees.len() as u32);

        let hot_trustee_addr_info: BtcTrusteeAddrInfo =
            create_multi_address::<T>(&hot_keys, sig_num).ok_or_else(|| {
                error!(
                    "[generate_trustee_session_info]|create hot_addr err!|hot_keys:{:?}",
                    hot_keys
                );
                Error::<T>::GenerateMultisigFailed
            })?;

        let cold_trustee_addr_info: BtcTrusteeAddrInfo =
            create_multi_address::<T>(&cold_keys, sig_num).ok_or_else(|| {
                error!(
                    "[generate_trustee_session_info]|create cold_addr err!|cold_keys:{:?}",
                    cold_keys
                );
                Error::<T>::GenerateMultisigFailed
            })?;

        native::info!(
            target: RUNTIME_TARGET,
            "[generate_trustee_session_info]|hot_addr:{:?}|cold_addr:{:?}|trustee_list:{:?}",
            hot_trustee_addr_info,
            cold_trustee_addr_info,
            trustees
        );

        Ok(TrusteeSessionInfo {
            trustee_list: trustees,
            threshold: sig_num as u16,
            hot_address: hot_trustee_addr_info,
            cold_address: cold_trustee_addr_info,
        })
    }
}

impl<T: Trait> Module<T> {
    pub fn ensure_trustee(who: &T::AccountId) -> DispatchResult {
        let trustee_session_info = trustee_session::<T>()?;
        if trustee_session_info.trustee_list.iter().any(|n| n == who) {
            return Ok(());
        }
        error!(
            "[ensure_trustee]|Committer not in the trustee list!|who:{:?}|trustees:{:?}",
            who, trustee_session_info.trustee_list
        );
        Err(Error::<T>::NotTrustee)?
    }

    pub fn apply_create_withdraw(
        who: T::AccountId,
        tx: Transaction,
        withdrawal_id_list: Vec<u32>,
    ) -> DispatchResult {
        let withdraw_amount = Self::max_withdrawal_count();
        if withdrawal_id_list.len() > withdraw_amount as usize {
            error!("[apply_create_withdraw]|Exceeding the maximum withdrawal amount|current list len:{:}|max:{:}", withdrawal_id_list.len(), withdraw_amount);
            Err(Error::<T>::WroungWithdrawalCount)?;
        }
        // remove duplicate
        let mut withdrawal_id_list = withdrawal_id_list;
        withdrawal_id_list.sort();
        withdrawal_id_list.dedup();

        check_withdraw_tx::<T>(&tx, &withdrawal_id_list)?;
        info!(
            "[apply_create_withdraw]|create new withdraw|withdrawal idlist:{:?}",
            withdrawal_id_list
        );

        // check sig
        let sigs_count = parse_and_check_signed_tx::<T>(&tx)?;
        let apply_sig = if sigs_count == 0 {
            false
        } else if sigs_count == 1 {
            true
        } else {
            error!("[apply_create_withdraw]|the sigs for tx could not more than 1 in apply_create_withdraw|current sigs:{:}", sigs_count);
            Err(Error::<T>::InvalidSigCount)?
        };

        xpallet_gateway_records::Module::<T>::process_withdrawal(
            Chain::Bitcoin,
            &withdrawal_id_list,
        )?;

        let mut proposal = BtcWithdrawalProposal::new(
            VoteResult::Unfinish,
            withdrawal_id_list.clone(),
            tx,
            Vec::new(),
        );

        info!("[apply_create_withdraw]|Through the legality check of withdrawal");

        Self::deposit_event(RawEvent::CreateWithdrawalProposal(
            who.clone(),
            withdrawal_id_list,
        ));

        if apply_sig {
            info!("[apply_create_withdraw]apply sign after create proposal");
            // due to `SignWithdrawalProposal` event should after `CreateWithdrawalProposal`, thus this function should after proposal
            // but this function would have an error return, this error return should not meet.
            if let Err(_) = insert_trustee_vote_state::<T>(true, &who, &mut proposal.trustee_list) {
                // should not be error in this function, if hit this branch, panic to clear all modification
                // TODO change to revoke in future
                panic!("insert_trustee_vote_state should not be error")
            }
        }

        WithdrawalProposal::<T>::put(proposal);

        Ok(())
    }

    pub fn apply_sig_withdraw(who: T::AccountId, tx: Option<Transaction>) -> DispatchResult {
        let mut proposal: BtcWithdrawalProposal<T::AccountId> =
            Self::withdrawal_proposal().ok_or(Error::<T>::NoProposal)?;

        if proposal.sig_state == VoteResult::Finish {
            error!("[apply_sig_withdraw]|proposal is on FINISH state, can't sign for this proposal|proposal：{:?}", proposal);
            Err(Error::<T>::RejectSig)?;
        }

        let (sig_num, total) = get_sig_num::<T>();
        match tx {
            Some(tx) => {
                // check this tx is same to proposal, just check input and output, not include sigs
                ensure_identical::<T>(&tx, &proposal.tx)?;

                // sign
                // check first and get signatures from commit transaction
                let sigs_count = parse_and_check_signed_tx::<T>(&tx)?;
                if sigs_count == 0 {
                    error!("[apply_sig_withdraw]|the tx sig should not be zero, zero is the source tx without any sig|tx{:?}", tx);
                    Err(Error::<T>::InvalidSigCount)?;
                }

                let confirmed_count = proposal
                    .trustee_list
                    .iter()
                    .filter(|(_, vote)| *vote == true)
                    .count() as u32;

                if sigs_count != confirmed_count + 1 {
                    error!(
                        "[apply_sig_withdraw]|Need to sign on the latest signature results|sigs count:{:}|confirmed count:{:}",
                        sigs_count,
                        confirmed_count
                    );
                    Err(Error::<T>::InvalidSigCount)?;
                }

                insert_trustee_vote_state::<T>(true, &who, &mut proposal.trustee_list)?;
                // check required count
                // required count should be equal or more than (2/3)*total
                // e.g. total=6 => required=2*6/3=4, thus equal to 4 should mark as finish
                if sigs_count == sig_num {
                    // mark as finish, can't do anything for this proposal
                    info!("[apply_sig_withdraw]Signature completed: {:}", sigs_count);
                    proposal.sig_state = VoteResult::Finish;

                    Self::deposit_event(RawEvent::FinishProposal(tx.hash()))
                } else {
                    proposal.sig_state = VoteResult::Unfinish;
                }
                // update tx
                proposal.tx = tx;
            }
            None => {
                // reject
                insert_trustee_vote_state::<T>(false, &who, &mut proposal.trustee_list)?;

                let reject_count = proposal
                    .trustee_list
                    .iter()
                    .filter(|(_, vote)| *vote == false)
                    .count() as u32;

                // reject count just need  < (total-required) / total
                // e.g. total=6 => required=2*6/3=4, thus, reject should more than (6-4) = 2
                // > 2 equal to total - required + 1 = 6-4+1 = 3
                let need_reject = total - sig_num + 1;
                if reject_count == need_reject {
                    info!(
                        "[apply_sig_withdraw]|{:}/{:} opposition, clear withdrawal propoal",
                        reject_count, total
                    );

                    // release withdrawal for applications
                    for id in proposal.withdrawal_id_list.iter() {
                        let _ = xpallet_gateway_records::Module::<T>::recover_withdrawal_by_trustee(
                            Chain::Bitcoin,
                            *id,
                        );
                    }

                    WithdrawalProposal::<T>::kill();

                    Self::deposit_event(RawEvent::DropWithdrawalProposal(
                        reject_count as u32,
                        sig_num as u32,
                        proposal.withdrawal_id_list.clone(),
                    ));
                    return Ok(());
                }
            }
        }

        info!(
            "[apply_sig_withdraw]|current sig|state:{:?}|trustee vote:{:?}",
            proposal.sig_state, proposal.trustee_list
        );

        WithdrawalProposal::<T>::put(proposal);
        Ok(())
    }
}

/// Get the required number of signatures
/// sig_num: Number of signatures required
/// trustee_num: Total number of multiple signatures
/// NOTE: Signature ratio greater than 2/3
pub fn get_sig_num<T: Trait>() -> (u32, u32) {
    let trustee_list = T::TrusteeSessionProvider::current_trustee_session()
        .map(|session_info| session_info.trustee_list)
        .expect("the trustee_list must exist; qed");
    let trustee_num = trustee_list.len() as u32;
    (two_thirds_unsafe(trustee_num), trustee_num)
}

fn create_multi_address<T: Trait>(
    pubkeys: &Vec<Public>,
    sig_num: u32,
) -> Option<BtcTrusteeAddrInfo> {
    let sum = pubkeys.len() as u32;
    if sig_num > sum {
        panic!("required sig num should less than trustee_num; qed")
    }
    if sum > 15 {
        error!("bitcoin's multisig can't more than 15, current is:{:}", sum);
        return None;
    }

    let opcode = match Opcode::from_u8(Opcode::OP_1 as u8 + sig_num as u8 - 1) {
        Some(o) => o,
        None => return None,
    };
    let mut build = Builder::default().push_opcode(opcode);
    for pubkey in pubkeys.iter() {
        build = build.push_bytes(&pubkey);
    }

    let opcode = match Opcode::from_u8(Opcode::OP_1 as u8 + sum as u8 - 1) {
        Some(o) => o,
        None => return None,
    };
    let redeem_script = build
        .push_opcode(opcode)
        .push_opcode(Opcode::OP_CHECKMULTISIG)
        .into_script();

    let addr = Address {
        kind: Type::P2SH,
        network: Module::<T>::network_id(),
        hash: dhash160(&redeem_script),
    };
    let script_bytes: Bytes = redeem_script.into();
    Some(BtcTrusteeAddrInfo {
        addr: addr2vecu8(&addr),
        redeem_script: script_bytes.into(),
    })
}

/// Update the signature status of trustee
/// state: false -> Veto signature, true -> Consent signature
/// only allow inseRelayedTx once
fn insert_trustee_vote_state<T: Trait>(
    state: bool,
    who: &T::AccountId,
    trustee_list: &mut Vec<(T::AccountId, bool)>,
) -> DispatchResult {
    match trustee_list.iter_mut().find(|ref info| info.0 == *who) {
        Some(_) => {
            // if account is exist, override state
            error!("[inseRelayedTx_trustee_vote_state]|already vote for this withdrawal proposal|who:{:?}|old vote:{:}", who, state);
            return Err("already vote for this withdrawal proposal")?;
        }
        None => {
            trustee_list.push((who.clone(), state));
            debug!(
                "[inseRelayedTx_trustee_vote_state]|inseRelayedTx new vote|who:{:?}|state:{:}",
                who, state
            );
        }
    }
    Module::<T>::deposit_event(RawEvent::SignWithdrawalProposal(who.clone(), state));
    Ok(())
}

/// Check that the cash withdrawal transaction is correct
fn check_withdraw_tx<T: Trait>(tx: &Transaction, withdrawal_id_list: &[u32]) -> DispatchResult {
    match Module::<T>::withdrawal_proposal() {
        Some(_) => Err(Error::<T>::NotFinishProposal)?,
        None => {
            // withdrawal addr list for account withdrawal application
            let mut appl_withdrawal_list: Vec<(Address, u64)> = Vec::new();
            for withdraw_index in withdrawal_id_list.iter() {
                let record =
                    xpallet_gateway_records::Module::<T>::pending_withdrawals(withdraw_index)
                        .ok_or(Error::<T>::NoWithdrawalRecord)?;
                // record.addr() is base58
                // verify btc address would conveRelayedTx a base58 addr to Address
                let addr: Address = Module::<T>::verify_btc_address(&record.addr())?;

                appl_withdrawal_list.push((addr, record.balance().saturated_into() as u64));
            }
            // not allow deposit directly to cold address, only hot address allow
            let hot_trustee_address: Address = get_hot_trustee_address::<T>()?;
            // withdrawal addr list for tx outputs
            let btc_withdrawal_fee = Module::<T>::btc_withdrawal_fee();
            let mut tx_withdraw_list = Vec::new();
            for output in tx.outputs.iter() {
                let script: Script = output.script_pubkey.clone().into();
                let addr = parse_output_addr::<T>(&script).ok_or("not found addr in this out")?;
                if addr.hash != hot_trustee_address.hash {
                    // expect change to trustee_addr output
                    tx_withdraw_list.push((addr, output.value + btc_withdrawal_fee));
                }
            }

            tx_withdraw_list.sort();
            appl_withdrawal_list.sort();

            // appl_withdrawal_list must match to tx_withdraw_list
            if appl_withdrawal_list.len() != tx_withdraw_list.len() {
                error!("withdrawal tx's outputs not equal to withdrawal application list, withdrawal application len:{:}, withdrawal tx's outputs len:{:}|withdrawal application list:{:?}, tx withdrawal outputs:{:?}",
                       appl_withdrawal_list.len(), tx_withdraw_list.len(),
                       withdrawal_id_list.iter().zip(appl_withdrawal_list).collect::<Vec<_>>(),
                       tx_withdraw_list
                );
                return Err(Error::<T>::InvalidProposal)?;
            }

            let count = appl_withdrawal_list.iter().zip(tx_withdraw_list).filter(|(a,b)|{
                if a.0 == b.0 && a.1 == b.1 {
                    return true
                }
                else {
                    error!(
                        "withdrawal tx's output not match to withdrawal application. withdrawal application :{:?}, tx withdrawal output:{:?}",
                        a,
                        b
                    );
                    return false
                }
            }).count();

            if count != appl_withdrawal_list.len() {
                Err(Error::<T>::InvalidProposal)?;
            }

            Ok(())
        }
    }
}