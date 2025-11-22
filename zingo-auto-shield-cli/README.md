# zingo-auto-shield-cli

This binary watches the chain for coinbase payouts that belong to a specific mnemonic, and automatically shields them once they reach a configurable confirmation depth.

## Configuration

Create a TOML file (pass its path with `--config`). A template lives in `zingo-auto-shield-cli/config.toml`; copy/edit it or start with the example below:

```toml
birthday = 0
chain = "mainnet"
lightwalletd_server = "http://127.0.0.1:9067"
rpc_url = "http://127.0.0.1:17777"
start_height = 0
confirmations = 10
# Directory used for the wallet + state files
state_dir = "/Users/me/.zcash/zingo-auto-shield"
processed_log = "processed_blocks.json"
rewards_log = "reward_blocks.json"
queue_log = "shield_queue.json"
log_file = "auto-shield.log"
# Poll interval once the daemon catches up (seconds)
poll_interval_seconds = 60
# Random delay (minutes) before shielding a detected reward
delay_min_minutes = 1440
delay_max_minutes = 14400
reschedule_grace_minutes = 5
# Optional: activation-height overrides for regtest/dev
# sapling_activation_height = 1
# orchard_activation_height = 1
```

* `birthday`: earliest block height for the wallet that receives coinbase payouts.
* `chain`: `mainnet` or `testnet`.
* `lightwalletd_server`: endpoint the wallet syncs against.
* `rpc_url`: full node RPC endpoint supporting `getblockcount`, `getblockhash`, and `getblock`.
* `start_height`: first block height to inspect.
* `confirmations`: how many blocks must confirm a coinbase payout before it is eligible for shielding.
* `state_dir`: base directory. The CLI stores the wallet, `processed_log`, and `rewards_log` files here (relative paths are resolved against this directory).
* `queue_log`: pending shield queue. Each detected reward is stored here until it executes.
* `log_file`: destination for the persistent log (stdout still sees the same entries).
* `poll_interval_seconds`: how long to sleep between scans once the daemon is caught up.
* `delay_min_minutes` / `delay_max_minutes`: randomized wait window applied to each reward before shielding. (Keeps observable shielding times de-correlated from block finds.)
* `reschedule_grace_minutes`: if the CLI restarts and finds a pending entry whose timer elapsed more than this many minutes ago, it re-rolls a new random delay before shielding (so downtime doesn’t reveal which miner was offline).
* `post_shield_send_address`: optional. When set, every successful shield queues a follow-up send of exactly 6.25 ZEC to this address using its own log (`post_shield_send_log`, default `post_shield_send_log.json`) and queue (`post_shield_send_queue_log`, default `post_shield_queue.json`). The follow-up send obeys its own randomized delay window (`post_shield_send_delay_min_minutes`/`post_shield_send_delay_max_minutes`; default 200-14,400 minutes) and reuses the same grace/retiming rules as the shield queue.
* `sapling_activation_height` / `orchard_activation_height`: optional overrides for local/dev networks—when set, the CLI programs the activation heights internally (no shell env tweaks needed).

`processed_log` records every block height the tool evaluated. `rewards_log` records the block hash / height pairs that matched the derived payout address and the txids produced by the automatic shield transaction.

## Operation

```
cargo run -p zingo-auto-shield-cli -- --config path/to/config.toml
```

On first launch (when no wallet exists under `state_dir`), the daemon prompts for your mnemonic via stdin (input is hidden). The phrase is never written to the config or log file and is dropped once the wallet is created. Subsequent restarts reuse the encrypted wallet state, so you only need to supply the mnemonic again if you delete that directory. Activation overrides from the config are applied automatically; no `ZINGO_*` environment variables are required.

Once running, the daemon:

1. Keeps the wallet synced to lightwalletd.
2. Fetches the current tip height via RPC and walks every height with `confirmations` blocks of finality.
3. Derives the expected transparent address for each height (index == block height) and compares it to the coinbase recipients.
4. When a match is found, it records the block in `queue_log` with a randomized execution time (between `delay_min_minutes` and `delay_max_minutes` minutes in the future).
5. Every processed height is appended to `processed_log`, allowing subsequent passes to resume where they left off. When the queue timer elapses *while the daemon is running*, the entry triggers a `quick_shield` restricted to that transparent index and the execution is appended to `rewards_log`. If the daemon wakes up long after the scheduled time, it re-rolls a new delay (respecting the grace window) before shielding so on-chain timing remains obfuscated.

If `post_shield_send_address` is populated, each successful shield also queues a fixed 6.25 ZEC send to that address. Those follow-up sends live in `post_shield_queue.json`, inherit their own randomized delay window, and write successful executions to `post_shield_send_log.json`. Missing funds, missing backend data, and overdue timers follow the same retry logic the shield queue uses.

If a queued shield fails because the funds are not yet spendable (`InsufficientFunds`), the daemon automatically postpones that entry for an additional `delay_max_minutes + 150` minutes so mature rewards can proceed before it retries.

The process stays alive until you press `Ctrl+C`. `poll_interval_seconds` controls how long it sleeps between scans once it has reached the tip.

## Address derivation model

The tool assumes your miner rotates transparent receivers by deriving child index == block height (i.e., block 1 → index 1, block 742 → index 742). Because `quick_shield` accepts a transparent restriction, the auto-shield run consumes *only* UTXOs credited to that child (even if older indices remain untouched).

If a block pays to an index that never saw funds (e.g., the miner skipped a height), nothing breaks—the wallet simply sees zero UTXOs for that child and moves on. You can safely re-run the tool later with a different `start_height` if you change your addressing strategy.

## Continuous operation

Because `processed_blocks.json`, `shield_queue.json`, `post_shield_queue.json`, `reward_blocks.json`, and (optionally) `post_shield_send_log.json` live under `state_dir`, each restart is idempotent—already-shielded heights are skipped automatically and pending entries resume their timers. The persistent log (`log_file`) continues across restarts, making it easy to audit what happened even after days of uptime.
