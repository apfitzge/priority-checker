use {
    clap::Parser,
    solana_client::{rpc_client::RpcClient, rpc_config::RpcBlockConfig},
    solana_sdk::{
        borsh0_10::try_from_slice_unchecked,
        clock::Slot,
        commitment_config::{CommitmentConfig, CommitmentLevel},
        compute_budget::{self, ComputeBudgetInstruction},
        pubkey::Pubkey,
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
    // let Ok(block) = client.get_block(slot) else {
    //     eprintln!("Failed to fetch block at slot {slot}");
    //     exit(1);
    // };
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
        .unwrap();

    let mut last_access_map: HashMap<Pubkey, LastAccessPriority> = HashMap::default();
    let mut violated_accounts: HashMap<Pubkey, Vec<[u64; 2]>> = HashMap::new();
    let mut violation_count = 0;

    let transactions = block.transactions.unwrap();
    for transaction in transactions {
        let mut is_violation = false;
        let Some(addresses) =
            Option::<UiLoadedAddresses>::from(transaction.meta.unwrap().loaded_addresses)
        else {
            eprintln!("Failed to fetch block at slot {slot}");
            exit(1);
        };

        let versioned_transaction = transaction.transaction.decode().unwrap();
        let sanitized_transaction =
            SanitizedVersionedTransaction::try_new(versioned_transaction).unwrap();
        let priority: u64 = get_priority(&sanitized_transaction);

        for write_account in addresses
            .writable
            .iter()
            .map(|k| Pubkey::from_str(k).unwrap())
        {
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

        for read_account in addresses
            .readonly
            .iter()
            .map(|k| Pubkey::from_str(k).unwrap())
        {
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
            violation_count += 1;
        }
    }

    if display_count_only {
        println!("{}", violation_count);
        return;
    }

    if violated_accounts.is_empty() {
        println!("No priority violations found");
    } else {
        println!(
            "{} priority violations found on {} accounts:",
            violation_count,
            violated_accounts.len()
        );
        for (account, violations) in violated_accounts {
            println!("Account: {}", account);
            for violation in violations {
                println!("  {} -> {}", violation[0], violation[1]);
            }
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
                        .unwrap()
                        .try_into()
                        .unwrap();
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
