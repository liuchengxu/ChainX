// Copyright 2019-2020 ChainX Project Authors. Licensed under GPL-3.0.

//! RPC interface for the transaction payment module.

use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::Arc;

use codec::Codec;
use jsonrpc_derive::rpc;

use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_runtime::{generic::BlockId, traits::Block as BlockT};

use xp_rpc::{
    hex_decode_error_into_rpc_err, runtime_error_into_rpc_err, trustee_decode_error_into_rpc_err,
    trustee_inexistent_error_into_rpc_err, Result,
};

use xpallet_support::RpcBalance;

use xpallet_gateway_common_rpc_runtime_api::trustees::bitcoin::{
    BtcTrusteeIntentionProps, BtcTrusteeSessionInfo,
};
use xpallet_gateway_common_rpc_runtime_api::{
    AssetId, Chain, GenericTrusteeIntentionProps, GenericTrusteeSessionInfo, WithdrawalLimit,
    XGatewayCommonApi as XGatewayCommonRuntimeApi,
};

/// XGatewayCommon RPC methods.
#[rpc]
pub trait XGatewayCommonApi<BlockHash, AccountId, Balance>
where
    Balance: Display + FromStr,
{
    /// Get bound addrs for an accountid
    #[rpc(name = "xgatewaycommon_boundAddrs")]
    fn bound_addrs(
        &self,
        who: AccountId,
        at: Option<BlockHash>,
    ) -> Result<BTreeMap<Chain, Vec<String>>>;

    /// Get withdrawal limit(minimal_withdrawal&fee) for an AssetId
    #[rpc(name = "xgatewaycommon_withdrawalLimit")]
    fn withdrawal_limit(
        &self,
        asset_id: AssetId,
        at: Option<BlockHash>,
    ) -> Result<WithdrawalLimit<RpcBalance<Balance>>>;

    /// Use the params to verify whether the withdrawal apply is valid. Notice those params is same as the params for call `XGatewayCommon::withdraw(...)`, including checking address is valid or something else. Front-end should use this rpc to check params first, than could create the extrinsic.
    #[rpc(name = "xgatewaycommon_verifyWithdrawal")]
    fn verify_withdrawal(
        &self,
        asset_id: AssetId,
        value: u64,
        addr: String,
        memo: String,
        at: Option<BlockHash>,
    ) -> Result<bool>;

    /// Return the trustee multisig address for all chain.
    #[rpc(name = "xgatewaycommon_trusteeMultisigs")]
    fn multisigs(&self, at: Option<BlockHash>) -> Result<BTreeMap<Chain, AccountId>>;

    /// Return bitcoin trustee registered property info for an account(e.g. registered hot/cold address)
    #[rpc(name = "xgatewaycommon_bitcoinTrusteeProperties")]
    fn btc_trustee_properties(
        &self,
        who: AccountId,
        at: Option<BlockHash>,
    ) -> Result<BtcTrusteeIntentionProps>;

    /// Return bitcoin trustee for current session(e.g. trustee hot/cold address and else)
    #[rpc(name = "xgatewaycommon_bitcoinTrusteeSessionInfo")]
    fn btc_trustee_session_info(
        &self,
        at: Option<BlockHash>,
    ) -> Result<BtcTrusteeSessionInfo<AccountId>>;

    /// Try to generate bitcoin trustee info for a list of candidates. (this api is used to check the trustee info which would be generated by those candidates)
    #[rpc(name = "xgatewaycommon_bitcoinGenerateTrusteeSessionInfo")]
    fn btc_generate_trustee_session_info(
        &self,
        candidates: Vec<AccountId>,
        at: Option<BlockHash>,
    ) -> Result<BtcTrusteeSessionInfo<AccountId>>;
}

/// A struct that implements the [`XStakingApi`].
pub struct XGatewayCommon<C, B, AccountId, Balance> {
    client: Arc<C>,
    _marker: std::marker::PhantomData<(B, AccountId, Balance)>,
}

impl<C, B, AccountId, Balance> XGatewayCommon<C, B, AccountId, Balance> {
    /// Create new `Contracts` with the given reference to the client.
    pub fn new(client: Arc<C>) -> Self {
        Self {
            client,
            _marker: Default::default(),
        }
    }
}

impl<C, Block, AccountId, Balance> XGatewayCommon<C, Block, AccountId, Balance>
where
    Block: BlockT,
    C: Send + Sync + 'static + ProvideRuntimeApi<Block> + HeaderBackend<Block>,
    C::Api: XGatewayCommonRuntimeApi<Block, AccountId, Balance>,
    AccountId: Codec + Send + Sync + 'static,
    Balance: Codec + Send + Sync + 'static,
{
    fn generic_trustee_properties(
        &self,
        chain: Chain,
        who: AccountId,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<GenericTrusteeIntentionProps> {
        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(|| self.client.info().best_hash));

        let result = api
            .trustee_properties(&at, chain, who)
            .map_err(runtime_error_into_rpc_err)?
            .ok_or(trustee_inexistent_error_into_rpc_err())?;

        Ok(result)
    }

    fn generic_trustee_session_info(
        &self,
        chain: Chain,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<GenericTrusteeSessionInfo<AccountId>> {
        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(|| self.client.info().best_hash));

        let result = api
            .trustee_session_info(&at, chain)
            .map_err(runtime_error_into_rpc_err)?
            .ok_or(trustee_inexistent_error_into_rpc_err())?;

        Ok(result)
    }

    fn generate_generic_trustee_session_info(
        &self,
        chain: Chain,
        candidates: Vec<AccountId>,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<GenericTrusteeSessionInfo<AccountId>> {
        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(|| self.client.info().best_hash));

        let result = api
            .generate_trustee_session_info(&at, chain, candidates)
            .map_err(runtime_error_into_rpc_err)?
            .map_err(runtime_error_into_rpc_err)?;

        Ok(result)
    }
}

impl<C, Block, AccountId, Balance> XGatewayCommonApi<<Block as BlockT>::Hash, AccountId, Balance>
    for XGatewayCommon<C, Block, AccountId, Balance>
where
    Block: BlockT,
    C: Send + Sync + 'static + ProvideRuntimeApi<Block> + HeaderBackend<Block>,
    C::Api: XGatewayCommonRuntimeApi<Block, AccountId, Balance>,
    AccountId: Codec + Send + Sync + 'static,
    Balance: Codec + Display + FromStr + Send + Sync + 'static + From<u64>,
{
    fn bound_addrs(
        &self,
        who: AccountId,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<BTreeMap<Chain, Vec<String>>> {
        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(||
            // If the block hash is not supplied assume the best block.
            self.client.info().best_hash));

        let result = api
            .bound_addrs(&at, who)
            .map_err(runtime_error_into_rpc_err)?;

        let result = result
            .into_iter()
            .filter_map(|(chain, addrs)| {
                let convert: Box<dyn Fn(Vec<u8>) -> String> = match chain {
                    Chain::Bitcoin => {
                        Box::new(|addr: Vec<u8>| String::from_utf8_lossy(&addr).into_owned())
                    }
                    Chain::Ethereum => Box::new(hex::encode),
                    _ => return None,
                };

                Some((chain, addrs.into_iter().map(|addr| convert(addr)).collect()))
            })
            .collect();

        Ok(result)
    }

    fn withdrawal_limit(
        &self,
        asset_id: AssetId,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<WithdrawalLimit<RpcBalance<Balance>>> {
        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(||
            // If the block hash is not supplied assume the best block.
            self.client.info().best_hash));

        let result = api
            .withdrawal_limit(&at, asset_id)
            .map_err(runtime_error_into_rpc_err)?
            .map(|src| WithdrawalLimit {
                minimal_withdrawal: src.minimal_withdrawal.into(),
                fee: src.fee.into(),
            })
            .map_err(runtime_error_into_rpc_err)?;
        Ok(result)
    }

    fn verify_withdrawal(
        &self,
        asset_id: AssetId,
        value: u64,
        addr: String,
        memo: String,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<bool> {
        let value: Balance = Balance::from(value);
        let addr = if addr.starts_with("0x") {
            hex::decode(&addr[2..]).map_err(hex_decode_error_into_rpc_err)?
        } else {
            hex::decode(&addr).unwrap_or_else(|_| addr.into_bytes())
        };
        let memo = memo.into_bytes();

        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(||
            // If the block hash is not supplied assume the best block.
            self.client.info().best_hash));
        Ok(api
            .verify_withdrawal(&at, asset_id, value, addr, memo.into())
            .map_err(runtime_error_into_rpc_err)?
            .is_ok())
    }

    fn multisigs(&self, at: Option<<Block as BlockT>::Hash>) -> Result<BTreeMap<Chain, AccountId>> {
        let api = self.client.runtime_api();
        let at = BlockId::hash(at.unwrap_or_else(||
                // If the block hash is not supplied assume the best block.
                self.client.info().best_hash));

        let result = api
            .trustee_multisigs(&at)
            .map_err(runtime_error_into_rpc_err)?;

        Ok(result)
    }

    fn btc_trustee_properties(
        &self,
        who: AccountId,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<BtcTrusteeIntentionProps> {
        let props = self.generic_trustee_properties(Chain::Bitcoin, who, at)?;
        BtcTrusteeIntentionProps::try_from(props).map_err(trustee_decode_error_into_rpc_err)
    }

    fn btc_trustee_session_info(
        &self,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<BtcTrusteeSessionInfo<AccountId>> {
        let info = self.generic_trustee_session_info(Chain::Bitcoin, at)?;
        BtcTrusteeSessionInfo::<_>::try_from(info).map_err(trustee_decode_error_into_rpc_err)
    }

    fn btc_generate_trustee_session_info(
        &self,
        candidates: Vec<AccountId>,
        at: Option<<Block as BlockT>::Hash>,
    ) -> Result<BtcTrusteeSessionInfo<AccountId>> {
        let info = self.generate_generic_trustee_session_info(Chain::Bitcoin, candidates, at)?;
        BtcTrusteeSessionInfo::<_>::try_from(info).map_err(trustee_decode_error_into_rpc_err)
    }
}
