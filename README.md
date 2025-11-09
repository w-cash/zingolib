## Zingolib
[![license](https://img.shields.io/github/license/zingolabs/zingolib)](LICENSE) [![codecov](https://codecov.io/gh/zingolabs/zingolib/branch/dev/graph/badge.svg?token=WMKTJMQY28)](https://codecov.io/gh/zingolabs/zingolib)
This repo provides both a library for zingo-mobile, as well as an included cli application to interact with zcashd via lightwalletd.

# Security Vulnerability Disclosure

If you believe you have discovered a security issue, please contact us at:

zingodisclosure@proton.me

## Zingo CLI
`zingo-cli` is a command line lightwalletd-proxy client. To use it, see "compiling from source" below. Releases are currently only provisional, we will update the README as releases come out.

## Privacy
* While all the keys and transaction detection happens on the client, the server can learn what blocks contain your shielded transactions.
* The server also learns other metadata about you like your ip address etc...
* Also remember that t-addresses are publicly visible on the blockchain.
* Price information is retrieved from Gemini exchange.

### Note Management
Zingo-CLI does automatic note and utxo management, which means it doesn't allow you to manually select which address to send outgoing transactions from. It follows these principles:
* Defaults to sending shielded transactions, even if you're sending to a transparent address
* Can select funds from multiple shielded addresses in the same transaction
* Will automatically shield your sapling funds at the first opportunity
    * When sending an outgoing transaction to a shielded address, Zingo-CLI can decide to use the transaction to additionally shield your sapling funds (i.e., send your sapling funds to your own orchard address in the same transaction)
* Transparent funds are only spent via explicit shield operations

## Index-Constrained Shielding & Sending

Use these steps to control which address index funds flow through.

### 1. Restore or create the wallet

```
cargo run -p zingo-cli --                         # interactive menu
cargo run -p zingo-cli -- --seed "<mnemonic>" --birthday <height> --data-dir <dir>
sync                                              # keep wallet at tip
```

### 2. Derive addresses for a specific index

| Type | Command |
|------|---------|
| Unified (orchard + sapling) | `new_address oz <index>` |
| Sapling-only | `new_address z <index>` |
| Transparent | `new_taddress` (sequential) / `new_taddress_allow_gap <index>` |

The CLI prints the encoded address plus the `address_index`. Use a fresh child for each deposit/coinbase.

### 3. Shield only one transparent index

```
shield '{"from":{"scope":"external","address_index":3,"max_address_index":3}}'
confirm            # or quickshield with the same JSON
```

`address_index` pins the starting child; `max_address_index` can match or extend it (e.g., allow 3‒10).

### 4. Inspect balances per index

```
balance               # global account totals
balance 3             # index-specific view
```

Transparent totals map directly to the child index. Sapling/Orchard balances appear only if you move shielded funds into an external UA at that index (see next step). Internal change is not listed per index.

### 5. Place shielded value at an external index

Shielding always deposits into an internal change address. To expose it at, say, index 100:

1. Derive the UA if you don’t already have it: `new_address oz 100`.
2. Send to yourself:
   ```
   send '{
     "receivers":[{"address":"<your index-100 UA>","amount":123456789}],
     "from":{"address_index":100,"max_address_index":100}   # optional
   }'
   confirm
   ```

After confirmation, `balance 100` shows the Sapling note, and you can constrain future sends with the same `from` selector.

### 6. Send funds (shielded → shielded/transparent)

```
send '{
  "receivers":[{"address":"u1recipient…","amount":1700000000}],
  "from":{"address_index":100,"max_address_index":100}
}'
confirm
```

To send to a transparent recipient, just supply a `t1…` address in `receivers`. The wallet still spends shielded notes and produces the desired transparent output.

> Transparent → transparent is *not* supported directly. Either shield first, then send, or import the mnemonic into a wallet that supports transparent RPC (e.g., `zcashd`).

### 7. Quick reference

| Action | Command |
|--------|---------|
| Shield only index N | `shield '{"from":{"scope":"external","address_index":N,"max_address_index":N}}'` |
| View balance for index N | `balance N` |
| Derive UA at index N | `new_address oz N` |
| Derive t-address at index N | `new_taddress_allow_gap N` |
| Send (shielded) from index N | `send '{"receivers":[…],"from":{"address_index":N,"max_address_index":N}}'` |
| One-shot shield | `quickshield '{"from":{…}}'` |

Remember to run `confirm` after any `shield`/`send` unless you used the `quick*` variants.

## Auto Shield CLI

`zingo-auto-shield-cli` automates the “derive address per block, wait for maturity, then quickshield” workflow. It restores the wallet from a mnemonic, scans finalized blocks, and when a block’s coinbase output matches the transparent child derived for that height, it automatically calls `quick_shield` constrained to that index. State is persisted so repeated runs only handle new blocks.

### Quick start

1. Create a config (defaults to `auto-shield-config.toml`). A ready-to-edit template lives at `zingo-auto-shield-cli/config.toml`:

    ```toml
    birthday = 0
    chain = "mainnet"                # or "testnet"
    lightwalletd_server = "http://127.0.0.1:9067"
    rpc_url = "http://127.0.0.1:17777"
    start_height = 0
    confirmations = 10
    state_dir = "/path/to/.zcash/zingo-auto-shield"
    processed_log = "processed_blocks.json"
    rewards_log = "reward_blocks.json"
    queue_log = "shield_queue.json"
    log_file = "auto-shield.log"
    poll_interval_seconds = 60
    delay_min_minutes = 1440
    delay_max_minutes = 14400
    reschedule_grace_minutes = 5
    # Optional overrides for regtest/dev (omit on mainnet):
    # sapling_activation_height = 1
    # orchard_activation_height = 1
    ```

2. Run `cargo run -p zingo-auto-shield-cli -- --config auto-shield-config.toml`.
   * On the very first launch (when the wallet directory is empty) the daemon prompts for your mnemonic on stdin; it is never persisted in the config and is dropped after the wallet is created. Subsequent restarts reuse the encrypted wallet on disk, so mnemonic entry is only needed if the wallet files are removed.
   * Activation height overrides in the config are applied automatically, so you no longer need to prefix the command with `ZINGO_*` env vars.
3. The process now stays running forever—no external cron loop required. It continually syncs, scans, and executes queued shields; `poll_interval_seconds` controls how long it sleeps between passes when the node is caught up.

Each detected coinbase match is queued with a randomized delay (`delay_min_minutes`‒`delay_max_minutes`). If the daemon was offline long enough that the timer already elapsed when it comes back, it assigns a brand-new random delay (respecting `reschedule_grace_minutes`) before touching the funds. Once the scheduled time is reached while the daemon is online, it performs a `quick_shield` restricted to that index; failures are re-enqueued with a fresh delay, so restarts simply pick up the backlog.

When quick_shield fails due to immature funds (`InsufficientFunds`), the daemon backs off more aggressively by delaying that entry for `delay_max_minutes + 150` minutes so other queued rewards can proceed.

All log output is duplicated to stdout and the configured `log_file`, so you can review what happened even after a long-running session.

See `zingo-auto-shield-cli/README.md` for detailed behaviour, file layout, and operational tips.

## Compiling from source

#### Pre-requisites
* Rust v1.85 or higher.
    * Run `rustup update` to get the latest version of Rust if you already have it installed
* Rustfmt
    * Run `rustup component add rustfmt` to add rustfmt
* Build Tools
    * Please install the build tools for your platform. On Ubuntu `sudo apt install build-essential gcc`
* Protobuf Compiler
    * Please install the protobuf compiler for your platform. On Ubuntu `sudo apt install protobuf-compiler`
* OpenSSL Dev
    * Please install development packages of openssl. On Ubuntu `sudo apt install libssl-dev`

```
git clone https://github.com/zingolabs/zingolib.git
cd zingolib
cargo build --release --package zingo-cli
./target/release/zingo-cli --data-dir /path/to/data_directory/
```

This will launch the interactive prompt. Type `help` to get a list of commands.

### Quick workflow (dev chain)

For short/lightweight chains, override activation heights before starting the CLI:

```bash
cd /path/to/zingolib
ZINGO_OVERWINTER_ACTIVATION_HEIGHT=1 \
ZINGO_SAPLING_ACTIVATION_HEIGHT=1 \
ZINGO_ORCHARD_ACTIVATION_HEIGHT=1 \
./target/release/zingo-cli \
    --server 127.0.0.1:9067 \
    --data-dir /path/to/wallet-data
```

Restoring from a seed requires a birthday:

```bash
ZINGO_OVERWINTER_ACTIVATION_HEIGHT=1 \
ZINGO_SAPLING_ACTIVATION_HEIGHT=1 \
ZINGO_ORCHARD_ACTIVATION_HEIGHT=1 \
./target/release/zingo-cli \
    --server 127.0.0.1:9067 \
    --data-dir /path/to/wallet-data \
    --seed "word1 … word24" \
    --birthday 0
```

Common command sequence inside the prompt:

1. `sync run` – ensure the background sync task is running.
2. `balance` – inspect confirmed/unconfirmed totals.
3. `coins` – list transparent UTXOs and the block height they were mined in.
4. Wait for coinbase rewards to mature (`coin_height + 100`) before shielding.
5. `shield` then `confirm` (or `quickshield`) – move transparent funds into Orchard.
6. `sync status` – monitor progress and confirmation counts.

> **Note:** Miner (coinbase) rewards cannot be spent until they are 100 blocks deep. Compare the height shown by `coins` with the current chain `height` to know when shielding will succeed.

## Notes:
* If you want to run your own server, please see [zingo lightwalletd](https://github.com/zingolabs/lightwalletd), and then run `./zingo-cli --server http://127.0.0.1:9067`
* The default log file is in `~/.zcash/zingo-wallet.debug.log`. A default wallet is stored in `~/.zcash/zingo-wallet.dat`
* Currently, the default, hard-coded `lightwalletd` server is https://mainnet.lightwalletd.com:9067/. To change this, you can modify the `DEFAULT_SERVER` const in `config/src/lib.rs`

## Running in non-interactive mode:
You can also run `zingo-cli` in non-interactive mode by passing the command you want to run as an argument. For example, `zingo-cli addresses` will list all wallet addresses and exit.
If you need to sync the wallet first before running the command, use --waitsync argument. This is useful for example for `zingo-cli balance`.
Run `zingo-cli help` to see a list of all commands.

## Options
Here are some CLI arguments you can pass to `zingo-cli`. Please run `zingo-cli --help` for the full list.

* `--data-dir`: uses the specified path as data directory. This is required when not using the `--regtest` option.
    * Example: `./zingo-cli --data-dir /path/to/data_directory/` will use the provided directory to store `zingo-wallet.dat` and logs. If the provided directory does not exist, it will create it.
* `--waitsync`: Wait for sync before running a command in non-interactive mode
    * Example: `./zingo-cli --data-dir /path/to/data_directory/ --waitsync balance`
* `--server`: Connect to a custom zcash lightwalletd server.
    * Example: `./zingo-cli --data-dir /path/to/data_directory/ --server 127.0.0.1:9067`
* `--seed`: Restore a wallet from a seed phrase. Note that this will fail if there is an existing wallet. Delete (or move) any existing wallet to restore from the 24-word seed phrase
* `--birthday`: Specify wallet birthday when restoring from seed. This is the earliest block height where the wallet has a transaction.
    * Example: `./zingo-cli --data-dir /path/to/data_directory/ --seed "twenty four words seed phrase" --birthday 1234567`
* `--recover`: Attempt to recover the seed phrase from a corrupted wallet

## Regtest
There is an experimental feature flag available with `zingo-cli`, in which the cli works in regtest mode, by also locally running `zcashd` and `lightwalletd`.

For a relatively recent user experience please see: https://free2z.cash/zingo-cli-in-regtest-mode

Please see `docs/TEST_REGTEST.md` for a detailed explanation.
