//! Read-only crypto wallets: public addresses the owner adds, stored in the
//! tables of schema 0023. Addresses are validated by
//! `jarvis_market_data::wallet`, which refuses anything that looks like key
//! material before any other check. There is no key, signing or transaction
//! path. Logs and audit rows name a wallet by id and kind, never by address.
//!
//! Off unless `JARVIS_CRYPTO_WALLETS_ENABLED=true`. When on, a background
//! task reads EUR prices and every wallet's balances every
//! [`REFRESH_INTERVAL`] (docs/CRYPTO_WALLETS.md); the owner may ask for a
//! pass at most once per [`REFRESH_COOLDOWN`].

pub mod store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jarvis_client_core::realtime::Event;
use jarvis_market_data::balances::{
    failure_code, Balance, CoinGecko, Esplora, EvmRpc, SolanaBalances,
};
use jarvis_market_data::rpc::RpcEndpoint;
use jarvis_market_data::wallet::{Chain, WalletKind};
use jarvis_market_data::MarketDataError;
use jarvis_store::Database;
use time::OffsetDateTime;

use crate::realtime::Hub;
use store::{CryptoStoreError, Refresh, Wallet};

/// Between passes.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Failed passes back off by doubling, up to this.
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// One whole pass; what it did not reach keeps its last state.
const PASS_DEADLINE: Duration = Duration::from_secs(60);
/// Between two refreshes the owner asks for (adding a wallet counts).
pub const REFRESH_COOLDOWN: Duration = Duration::from_secs(60);
const DB_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared by the refresh task and the routes.
#[derive(Default)]
pub struct CryptoHandle {
    enabled: bool,
    /// The chains with a configured, accepted source.
    chains: Vec<Chain>,
    last_request: Mutex<Option<Instant>>,
    wake: tokio::sync::Notify,
    /// A wake is scheduled for the end of the cooldown.
    wake_scheduled: AtomicBool,
}

impl CryptoHandle {
    /// Turned on, with these chains available. [`Default`] is off.
    pub fn new(chains: Vec<Chain>) -> Self {
        Self {
            enabled: true,
            chains,
            ..Self::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn chains(&self) -> &[Chain] {
        &self.chains
    }

    /// Queue a pass, unless one was asked for less than [`REFRESH_COOLDOWN`]
    /// ago: then the whole seconds left.
    pub fn request_refresh(&self) -> Result<(), u64> {
        let now = Instant::now();
        let mut last = self.last_request.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(waited) = last.map(|at| now.duration_since(at)) {
            if waited < REFRESH_COOLDOWN {
                return Err((REFRESH_COOLDOWN - waited).as_secs().max(1));
            }
        }
        *last = Some(now);
        self.wake.notify_one();
        Ok(())
    }

    /// [`Self::request_refresh`], or one pass when the cooldown ends (a
    /// burst of calls schedules a single one).
    pub fn refresh_soon(self: &Arc<Self>) {
        let Err(wait) = self.request_refresh() else {
            return;
        };
        if self.wake_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let handle = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            handle.wake_scheduled.store(false, Ordering::Release);
            let _ = handle.request_refresh();
        });
    }
}

/// The balance and price sources of a pass.
pub struct Sources {
    prices: CoinGecko,
    bitcoin: Esplora,
    evm: Vec<Arc<EvmRpc>>,
    solana: Option<SolanaBalances>,
}

/// Build the handle and, when turned on, the sources. Bitcoin is always
/// available; an EVM chain when its URL is set and accepted; Solana when the
/// primary market RPC URL is. A refused URL is logged by chain only.
pub fn configure(
    enabled: bool,
    solana_rpc_url: &str,
    evm_rpc_urls: [(Chain, &str); 5],
) -> (CryptoHandle, Option<Sources>) {
    if !enabled {
        return (CryptoHandle::default(), None);
    }
    let (prices, bitcoin) = match (CoinGecko::new(), Esplora::new()) {
        (Ok(prices), Ok(bitcoin)) => (prices, bitcoin),
        (Err(error), _) | (_, Err(error)) => {
            tracing::error!(%error, "crypto wallets disabled");
            return (CryptoHandle::default(), None);
        }
    };
    let endpoint = |chain: Chain, url: &str| -> Option<RpcEndpoint> {
        let url = url.trim();
        if url.is_empty() {
            return None;
        }
        RpcEndpoint::new(url.to_owned())
            .inspect_err(|error| tracing::error!(?chain, %error, "crypto RPC URL refused"))
            .ok()
    };
    let mut chains = vec![Chain::Bitcoin];
    let mut evm = Vec::new();
    for (chain, url) in evm_rpc_urls {
        let client = endpoint(chain, url).and_then(|endpoint| {
            EvmRpc::new(chain, endpoint)
                .inspect_err(|error| tracing::error!(?chain, %error, "crypto RPC refused"))
                .ok()
        });
        if let Some(client) = client {
            chains.push(chain);
            evm.push(Arc::new(client));
        }
    }
    let solana = endpoint(Chain::Solana, solana_rpc_url).and_then(|endpoint| {
        SolanaBalances::new(endpoint)
            .inspect_err(|error| tracing::error!(%error, "crypto Solana RPC refused"))
            .ok()
    });
    if solana.is_some() {
        chains.push(Chain::Solana);
    }
    (
        CryptoHandle::new(chains),
        Some(Sources {
            prices,
            bitcoin,
            evm,
            solana,
        }),
    )
}

/// Spawn the refresh task: a pass right away, then every
/// [`REFRESH_INTERVAL`] or when asked. Failed passes back off; every pass
/// ends with `trading.crypto.updated`.
pub fn start(db: Database, handle: Arc<CryptoHandle>, sources: Sources, events: Hub) {
    tokio::spawn(async move {
        let mut failures: u32 = 0;
        let mut delay = Duration::ZERO;
        loop {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = handle.wake.notified() => {}
            }
            let ok = match tokio::time::timeout(PASS_DEADLINE, pass(&db, &sources)).await {
                Ok(ok) => ok,
                Err(_) => {
                    tracing::warn!("crypto refresh pass hit its deadline");
                    false
                }
            };
            events.publish_all(Event::CryptoUpdated {
                as_of: OffsetDateTime::now_utc(),
            });
            failures = if ok { 0 } else { (failures + 1).min(2) };
            delay = REFRESH_INTERVAL
                .saturating_mul(1 << failures)
                .min(MAX_BACKOFF);
        }
    });
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, CryptoStoreError>>,
) -> Result<T, CryptoStoreError> {
    tokio::time::timeout(DB_TIMEOUT, future)
        .await
        .unwrap_or(Err(CryptoStoreError::Database))
}

/// Prices, then each wallet. False when any read or write failed.
// ponytail: one provider's rate limit backs off the whole pass; per-provider
// schedules if that keeps other chains stale.
async fn pass(db: &Database, sources: &Sources) -> bool {
    let mut ok = true;
    match sources.prices.prices().await {
        Ok(prices) => {
            if let Err(error) = bounded(store::write_prices(db, &prices)).await {
                tracing::warn!(%error, "crypto prices could not be stored");
                ok = false;
            }
        }
        Err(error) => {
            tracing::warn!(code = failure_code(&error), "crypto price refresh failed");
            ok = false;
        }
    }
    let wallets = match bounded(store::list(db)).await {
        Ok(wallets) => wallets,
        Err(error) => {
            tracing::warn!(%error, "crypto wallets could not be read");
            return false;
        }
    };
    for wallet in &wallets {
        let read = read_wallet(sources, wallet).await;
        let kind = kind_name(wallet.kind);
        if let Some(code) = read.error {
            tracing::warn!(wallet = %wallet.id, kind, code, "crypto wallet refresh failed");
            ok &= !read.counts_as_failure;
        }
        let refresh = Refresh {
            chains: &read.chains,
            balances: &read.balances,
            other_tokens: read.other_tokens,
            error: read.error,
        };
        match bounded(store::write_refresh(db, &wallet.id, &refresh)).await {
            Ok(()) | Err(CryptoStoreError::NotFound) => {}
            Err(error) => {
                tracing::warn!(wallet = %wallet.id, kind, %error, "crypto wallet refresh not stored");
                ok = false;
            }
        }
    }
    ok
}

fn kind_name(kind: WalletKind) -> &'static str {
    match kind {
        WalletKind::Bitcoin => "bitcoin",
        WalletKind::Evm => "evm",
        WalletKind::Solana => "solana",
    }
}

#[derive(Default)]
struct WalletRead {
    chains: Vec<Chain>,
    balances: Vec<Balance>,
    other_tokens: Option<u32>,
    error: Option<&'static str>,
    /// False when the only failure is a chain without a configured source:
    /// that does not back off the pass.
    counts_as_failure: bool,
}

impl WalletRead {
    fn fail(&mut self, code: &'static str, counts: bool) {
        self.error.get_or_insert(code);
        self.counts_as_failure |= counts;
    }

    fn absorb(&mut self, chain: Chain, read: Result<Vec<Balance>, MarketDataError>) {
        match read {
            Ok(balances) => {
                self.chains.push(chain);
                self.balances.extend(balances);
            }
            Err(error) => self.fail(failure_code(&error), true),
        }
    }
}

/// Every chain of one wallet; EVM chains in parallel (each provider has its
/// own pacer).
async fn read_wallet(sources: &Sources, wallet: &Wallet) -> WalletRead {
    let mut read = WalletRead::default();
    match wallet.kind {
        WalletKind::Bitcoin => {
            let balance = sources.bitcoin.balance(&wallet.address).await;
            read.absorb(Chain::Bitcoin, balance.map(|b| vec![b]));
        }
        WalletKind::Solana => match &sources.solana {
            Some(solana) => match solana.read(&wallet.address).await {
                Ok(holdings) => {
                    read.other_tokens = Some(holdings.other_tokens);
                    read.absorb(Chain::Solana, Ok(holdings.balances));
                }
                Err(error) => read.absorb(Chain::Solana, Err(error)),
            },
            None => read.fail("unavailable", false),
        },
        WalletKind::Evm => {
            let mut tasks = tokio::task::JoinSet::new();
            for chain in &wallet.chains {
                match sources.evm.iter().find(|client| client.chain() == *chain) {
                    Some(client) => {
                        let (client, chain, address) =
                            (client.clone(), *chain, wallet.address.clone());
                        tasks.spawn(async move { (chain, client.balances(&address).await) });
                    }
                    None => read.fail("unavailable", false),
                }
            }
            while let Some(joined) = tasks.join_next().await {
                match joined {
                    Ok((chain, balances)) => read.absorb(chain, balances),
                    Err(_) => read.fail("unavailable", true),
                }
            }
        }
    }
    read
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_requests_are_spaced_by_the_cooldown() {
        let handle = CryptoHandle::new(vec![Chain::Bitcoin]);
        assert!(handle.request_refresh().is_ok());
        let wait = handle.request_refresh().unwrap_err();
        assert!((59..=60).contains(&wait), "{wait}");
        *handle.last_request.lock().unwrap() =
            Some(Instant::now() - REFRESH_COOLDOWN - Duration::from_secs(1));
        assert!(handle.request_refresh().is_ok());
    }

    #[tokio::test]
    async fn a_refused_request_runs_once_the_cooldown_ends() {
        let handle = Arc::new(CryptoHandle::new(vec![Chain::Bitcoin]));
        handle.refresh_soon();
        handle.wake.notified().await;
        // One second of cooldown left.
        *handle.last_request.lock().unwrap() =
            Some(Instant::now() - REFRESH_COOLDOWN + Duration::from_secs(1));
        handle.refresh_soon();
        handle.refresh_soon();
        assert!(handle.wake_scheduled.load(Ordering::Acquire));
        tokio::time::timeout(Duration::from_secs(5), handle.wake.notified())
            .await
            .expect("woken after the cooldown");
        assert!(!handle.wake_scheduled.load(Ordering::Acquire));
    }

    #[test]
    fn chains_follow_the_configuration() {
        let off = configure(false, "", [(Chain::Ethereum, ""); 5]);
        assert!(!off.0.enabled() && off.1.is_none());
        let (handle, sources) = configure(
            true,
            "https://rpc.evil.example/?api-key=x",
            [
                (Chain::Ethereum, "https://ethereum-rpc.publicnode.com"),
                (Chain::Base, ""),
                (Chain::Arbitrum, "https://arbitrum-one-rpc.publicnode.com"),
                (Chain::Optimism, "https://evil.example"),
                (Chain::Polygon, "  "),
            ],
        );
        assert!(handle.enabled() && sources.is_some());
        assert_eq!(
            handle.chains(),
            [Chain::Bitcoin, Chain::Ethereum, Chain::Arbitrum]
        );
        let (handle, _) = configure(
            true,
            "https://api.mainnet-beta.solana.com",
            [
                (Chain::Ethereum, ""),
                (Chain::Base, ""),
                (Chain::Arbitrum, ""),
                (Chain::Optimism, ""),
                (Chain::Polygon, ""),
            ],
        );
        assert_eq!(handle.chains(), [Chain::Bitcoin, Chain::Solana]);
    }
}
