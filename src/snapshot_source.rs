use jsonrpc_core_client::transports::http;

use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
    rpc_response::{Response, RpcKeyedAccount},
};
use solana_rpc::{rpc::rpc_full::FullClient, rpc::OptionalContext};
use solana_sdk::{account::AccountSharedData, commitment_config::CommitmentConfig, pubkey::Pubkey};

use log::*;
use std::str::FromStr;
use tokio::time;

use crate::{AnyhowWrap, Config};

#[derive(Clone)]
pub struct AccountSnapshot {
    pub slot: u64,
    pub accounts: Vec<AccountSnapshotData>,
}

#[derive(Clone)]
pub struct AccountSnapshotData {
    pub pubkey: Pubkey,
    pub account: AccountSharedData,
}

impl AccountSnapshot {
    pub fn from_rpc(rpc: Response<Vec<RpcKeyedAccount>>) -> anyhow::Result<Self> {
        Ok(Self {
            slot: rpc.context.slot,
            accounts: rpc
                .value
                .iter()
                .map(|a| {
                    Ok(AccountSnapshotData {
                        pubkey: Pubkey::from_str(&a.pubkey).unwrap(),
                        account: a
                            .account
                            .decode()
                            .ok_or(anyhow::anyhow!("could not decode account"))?,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
        })
    }
}

async fn feed_snapshots(
    config: &Config,
    sender: &async_channel::Sender<AccountSnapshot>,
) -> anyhow::Result<()> {
    let mango_program_id = Pubkey::from_str(&config.mango_program_id)?;
    let serum_program_id = Pubkey::from_str(&config.serum_program_id)?;
    let mango_signer_id = Pubkey::from_str(&config.mango_signer_id)?;

    let rpc_client = http::connect_with_options::<FullClient>(&config.rpc_http_url, true)
        .await
        .map_err_anyhow()?;

    let account_info_config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::processed()),
        data_slice: None,
    };
    let all_accounts_config = RpcProgramAccountsConfig {
        filters: None,
        with_context: Some(true),
        account_config: account_info_config.clone(),
    };
    let open_orders_accounts_config = RpcProgramAccountsConfig {
        // filter for only OpenOrders with mango_signer as owner
        filters: Some(vec![
            RpcFilterType::DataSize(3228), // open orders size
            RpcFilterType::Memcmp(Memcmp {
                offset: 0,
                // "serum" + u64 that is Initialized (1) + OpenOrders (4)
                bytes: MemcmpEncodedBytes::Base58("AcUQf4PGf6fCHGwmpB".into()),
                encoding: None,
            }),
            RpcFilterType::Memcmp(Memcmp {
                offset: 45, // owner is the 4th field, after "serum" (header), account_flags: u64 and market: Pubkey
                bytes: MemcmpEncodedBytes::Bytes(mango_signer_id.to_bytes().into()),
                encoding: None,
            }),
        ]),
        with_context: Some(true),
        account_config: account_info_config.clone(),
    };

    // TODO: This way the snapshots are done sequentially, and a failing snapshot prohibits the second one to be attempted

    let account_snapshot = rpc_client
        .get_program_accounts(
            mango_program_id.to_string(),
            Some(all_accounts_config.clone()),
        )
        .await
        .map_err_anyhow()?;
    if let OptionalContext::Context(account_snapshot_response) = account_snapshot {
        sender
            .send(AccountSnapshot::from_rpc(account_snapshot_response)?)
            .await
            .expect("sending must succeed");
    } else {
        anyhow::bail!("did not receive context");
    }

    let account_snapshot = rpc_client
        .get_program_accounts(
            serum_program_id.to_string(),
            Some(open_orders_accounts_config.clone()),
        )
        .await
        .map_err_anyhow()?;
    if let OptionalContext::Context(account_snapshot_response) = account_snapshot {
        sender
            .send(AccountSnapshot::from_rpc(account_snapshot_response)?)
            .await
            .expect("sending must succeed");
    } else {
        anyhow::bail!("did not receive context");
    }

    Ok(())
}

pub fn start(config: Config, sender: async_channel::Sender<AccountSnapshot>) {
    let mut interval = time::interval(time::Duration::from_secs(180));

    tokio::spawn(async move {
        loop {
            interval.tick().await;
            if let Err(err) = feed_snapshots(&config, &sender).await {
                warn!("snapshot error: {:?}", err);
            } else {
                info!("snapshot success");
            };
        }
    });
}
