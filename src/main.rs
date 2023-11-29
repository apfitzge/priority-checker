use {
    clap::Parser,
    solana_client::{rpc_client::RpcClient, rpc_config::RpcBlockConfig},
    solana_sdk::{
        borsh0_10::try_from_slice_unchecked,
        clock::Slot,
        commitment_config::{CommitmentConfig, CommitmentLevel},
        compute_budget::{self, ComputeBudgetInstruction},
        pubkey::Pubkey,
        signature::Signature,
        transaction::SanitizedVersionedTransaction,
    },
    solana_transaction_status::{TransactionDetails, UiLoadedAddresses, UiTransactionEncoding},
    std::{
        collections::{hash_map::Entry, HashMap},
        process::exit,
        str::FromStr,
    },
};

#[derive(Debug, Parser)]
struct Cli {
    /// Slot to fetch block and perform priority checks for.
    slot: Slot,
    /// Display number of violations only.
    #[clap(short = 'c', long, default_value_t = false)]
    display_count_only: bool,
}

fn main() {
    let Cli {
        slot,
        display_count_only,
    } = Cli::parse();

    let client = RpcClient::new("https://api.mainnet-beta.solana.com");
    let block = client
        .get_block_with_config(
            slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::Binary),
                transaction_details: Some(TransactionDetails::Full),
                rewards: None,
                commitment: Some(CommitmentConfig {
                    commitment: CommitmentLevel::Confirmed,
                }),
                max_supported_transaction_version: Some(0),
            },
        )
        .unwrap_or_else(|err| {
            eprintln!("Failed to fetch block at slot {}: {}", slot, err);
            exit(1);
        });

    let mut last_access_map: HashMap<Pubkey, LastAccessPriority> = HashMap::default();
    let mut violated_accounts: HashMap<Pubkey, Vec<[u64; 2]>> = HashMap::new();
    let mut violating_transaction_signatures: Vec<Signature> = Vec::new();

    let transactions = block.transactions.unwrap_or_else(|| {
        eprintln!("Block does not have transactions, something is misconfigured");
        exit(1);
    });
    for transaction in transactions {
        let mut is_violation = false;
        let Some(addresses) = Option::<UiLoadedAddresses>::from(
            transaction
                .meta
                .unwrap_or_else(|| {
                    eprintln!("Transactions do not have metadata, something is misconfigured");
                    exit(1);
                })
                .loaded_addresses,
        ) else {
            eprintln!("Transactions do not have loaded addresses, something is misconfigured");
            exit(1);
        };

        let versioned_transaction = transaction.transaction.decode().unwrap_or_else(|| {
            eprintln!("Failed to decode transaction");
            exit(1);
        });
        let signature = versioned_transaction.signatures[0];
        let sanitized_transaction = SanitizedVersionedTransaction::try_new(versioned_transaction)
            .unwrap_or_else(|err| {
                eprintln!("Failed to sanitize transaction: {err}");
                exit(1);
            });
        let priority: u64 = get_priority(&sanitized_transaction);

        for write_account in addresses.writable.iter().map(|k| {
            Pubkey::from_str(k).unwrap_or_else(|err| {
                eprintln!("Failed to parse pubkey {k}: {err}");
                exit(1);
            })
        }) {
            match last_access_map.entry(write_account) {
                Entry::Occupied(mut entry) => {
                    if entry.get().priority < priority {
                        is_violation = true;
                        violated_accounts
                            .entry(write_account)
                            .or_default()
                            .push([entry.get().priority, priority]);
                    }

                    entry.insert(LastAccessPriority {
                        last_access: LastAccess::Write,
                        priority,
                    });
                }
                Entry::Vacant(entry) => {
                    entry.insert(LastAccessPriority {
                        last_access: LastAccess::Write,
                        priority,
                    });
                }
            }
        }

        for read_account in addresses.readonly.iter().map(|k| {
            Pubkey::from_str(k).unwrap_or_else(|err| {
                eprintln!("Failed to parse pubkey {k}: {err}");
                exit(1);
            })
        }) {
            match last_access_map.entry(read_account) {
                Entry::Occupied(mut entry) => {
                    if entry.get().last_access == LastAccess::Write
                        && entry.get().priority < priority
                    {
                        is_violation = true;
                        violated_accounts
                            .entry(read_account)
                            .or_default()
                            .push([entry.get().priority, priority]);
                    }

                    entry.insert(LastAccessPriority {
                        last_access: LastAccess::Read,
                        priority,
                    });
                }
                Entry::Vacant(entry) => {
                    entry.insert(LastAccessPriority {
                        last_access: LastAccess::Read,
                        priority,
                    });
                }
            }
        }

        if is_violation {
            violating_transaction_signatures.push(signature);
        }
    }

    if display_count_only {
        println!("{}", violating_transaction_signatures.len());
        return;
    }

    if violated_accounts.is_empty() {
        println!("No priority violations found");
    } else {
        println!(
            "{} priority violations found on {} accounts:",
            violating_transaction_signatures.len(),
            violated_accounts.len()
        );
        for (account, violations) in violated_accounts {
            println!("Account: {}", account);
            for violation in violations {
                println!("  {} -> {}", violation[0], violation[1]);
            }
        }
        println!("Violating transactions:");
        for signature in violating_transaction_signatures {
            println!("{}", signature);
        }
    }
}

fn get_priority(transaction: &SanitizedVersionedTransaction) -> u64 {
    for (program_id, ix) in transaction.get_message().program_instructions_iter() {
        if compute_budget::check_id(program_id) {
            match try_from_slice_unchecked(&ix.data) {
                Ok(ComputeBudgetInstruction::RequestUnitsDeprecated {
                    units,
                    additional_fee,
                }) => {
                    const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;
                    return (additional_fee as u128)
                        .saturating_mul(MICRO_LAMPORTS_PER_LAMPORT)
                        .checked_div(units as u128)
                        .unwrap_or_else(|| {
                            eprintln!("Failed to calculate priority");
                            exit(1);
                        })
                        .try_into()
                        .unwrap_or_else(|err| {
                            eprintln!("Failed to calculate priority: {err}");
                            exit(1);
                        });
                }
                Ok(ComputeBudgetInstruction::SetComputeUnitPrice(price)) => {
                    return price;
                }
                _ => {}
            }
        }
    }

    0
}

#[derive(PartialEq, Eq)]
enum LastAccess {
    Read,
    Write,
}

struct LastAccessPriority {
    last_access: LastAccess,
    priority: u64,
}
