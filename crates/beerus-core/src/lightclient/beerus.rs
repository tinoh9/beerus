use std::{collections::BTreeMap, str::FromStr, sync::Arc, thread, time};
use tokio::sync::RwLock;

use super::{ethereum::EthereumLightClient, starknet::StarkNetLightClient};
use crate::{config::Config, ethers_helper};
use ethers::{
    abi::Abi,
    types::{H160, U256},
};
use eyre::Result;
use helios::types::{BlockTag, CallOpts};
use log::{error, info, warn};
use starknet::{
    core::types::FieldElement,
    providers::jsonrpc::models::{
        BlockHashAndNumber, BlockId, BlockStatus, BlockTag as StarknetBlockTag, BlockWithTxHashes,
        BlockWithTxs, BroadcastedTransaction, DeclareTransaction, DeployAccountTransaction,
        DeployTransaction, FeeEstimate, FunctionCall, InvokeTransaction, L1HandlerTransaction,
        MaybePendingBlockWithTxHashes, MaybePendingBlockWithTxs, MaybePendingTransactionReceipt,
        Transaction,
    },
};

/// Enum representing the different synchronization status of the light client.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    NotSynced,
    Syncing,
    Synced,
}

#[derive(Clone, Debug)]
pub struct NodeData {
    pub block_number: u64,
    pub state_root: String,
    pub payload: BTreeMap<u64, BlockWithTxs>,
}

impl NodeData {
    pub fn new() -> Self {
        NodeData {
            block_number: 0,
            state_root: "".to_string(),
            payload: BTreeMap::new(),
        }
    }
}

impl Default for NodeData {
    fn default() -> Self {
        Self::new()
    }
}

/// Beerus Light Client service.
pub struct BeerusLightClient {
    /// Global configuration.
    pub config: Config,
    /// Ethereum light client.
    pub ethereum_lightclient: Arc<RwLock<Box<dyn EthereumLightClient>>>,
    /// StarkNet light client.
    pub starknet_lightclient: Arc<Box<dyn StarkNetLightClient>>,
    /// Sync status.
    pub sync_status: SyncStatus,
    /// StarkNet core ABI.
    pub starknet_core_abi: Abi,
    /// StarkNet core contract address.
    pub starknet_core_contract_address: H160,
    // TODO: Add Payload data
    pub node: Arc<RwLock<NodeData>>,
}

impl BeerusLightClient {
    /// Create a new Beerus Light Client service.
    pub fn new(
        config: Config,
        //TODO: Check if we should just have &str as arguments
        ethereum_lightclient_raw: Box<dyn EthereumLightClient>,
        starknet_lightclient_raw: Box<dyn StarkNetLightClient>,
    ) -> Self {
        // Create a new Ethereum light client.
        let ethereum_lightclient = Arc::new(RwLock::new(ethereum_lightclient_raw));
        // Create a new StarkNet light client.
        let starknet_lightclient = Arc::new(starknet_lightclient_raw);
        let starknet_core_abi = include_str!("../resources/starknet_core_abi.json");
        // Deserialize the StarkNet core ABI.
        // For now we assume that the ABI is valid and that the deserialization will never fail.
        let starknet_core_abi: Abi = serde_json::from_str(starknet_core_abi).unwrap();
        let starknet_core_contract_address = config.starknet_core_contract_address;
        let node_raw = NodeData::new();
        let node = Arc::new(RwLock::new(node_raw));

        Self {
            config,
            ethereum_lightclient,
            starknet_lightclient,
            sync_status: SyncStatus::NotSynced,
            starknet_core_abi,
            starknet_core_contract_address,
            node,
        }
    }

    /// Start Beerus light client and synchronize with Ethereum and StarkNet.
    pub async fn start(&mut self) -> Result<()> {
        if let SyncStatus::NotSynced = self.sync_status {
            // Start the Ethereum light client.
            self.ethereum_lightclient.write().await.start().await?;
            // Start the StarkNet light client.
            self.starknet_lightclient.start().await?;
            self.sync_status = SyncStatus::Synced;
            let ethereum_clone = self.ethereum_lightclient.clone();
            let starknet_clone = self.starknet_lightclient.clone();
            let node_clone = self.node.clone();

            // Define function that will loop
            let task = async move {
                loop {
                    let state_root = ethereum_clone
                        .read()
                        .await
                        .starknet_state_root()
                        .await
                        .unwrap();

                    let last_proven_block = ethereum_clone
                        .read()
                        .await
                        .starknet_last_proven_block()
                        .await
                        .unwrap();

                    // TODO: these logs don't get caught by the main thread
                    info!("State Root: {state_root}");
                    info!("Block Number: {last_proven_block}");

                    match starknet_clone
                        .get_block_with_txs(&BlockId::Tag(StarknetBlockTag::Latest))
                        .await
                    {
                        Ok(block) => {
                            println!("block: {:?}", block);
                            let mut data = node_clone.write().await;
                            match block {
                                MaybePendingBlockWithTxs::Block(block) => {
                                    // if block.block_number > data.block_number && block.block_number == last_proven_block
                                    if block.block_number > data.block_number
                                        && 0 < block.block_number
                                    {
                                        data.block_number = block.block_number;
                                        data.state_root = block.new_root.to_string();
                                        data.payload.insert(block.block_number, block);
                                        info!("New Block Added to Payload:");
                                        info!("Block Number {:?}", &data.block_number);
                                        info!("Block Root {:?}", &data.state_root);
                                    }
                                }
                                MaybePendingBlockWithTxs::PendingBlock(_) => {
                                    warn!("Pending Block");
                                }
                            }
                        }
                        Err(err) => {
                            error!("Error getting block: {}", err);
                        }
                    }
                    //TODO: Make this configurable
                    thread::sleep(time::Duration::from_secs(5));
                }
            };
            // Spawn loop function
            tokio::spawn(task);
        };
        Ok(())
    }

    /// Return the current synchronization status.
    pub fn sync_status(&self) -> &SyncStatus {
        &self.sync_status
    }

    /// Get the storage at a given address/key.
    /// This function is used to get the storage at a given address and key.
    ///
    /// # Arguments
    ///
    /// * `contract_address` - The StarkNet contract address.
    /// * `storage_key` - The storage key.
    ///
    /// # Returns
    ///
    /// `Ok(FieldElement)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_get_storage_at(
        &self,
        contract_address: FieldElement,
        storage_key: FieldElement,
    ) -> Result<FieldElement> {
        let last_block = self
            .ethereum_lightclient
            .read()
            .await
            .starknet_last_proven_block()
            .await?
            .as_u64();
        self.starknet_lightclient
            .get_storage_at(contract_address, storage_key, last_block)
            .await
    }

    /// Call starknet contract view.
    /// This function is used to call a view function of a StarkNet contract.
    /// WARNING: This function is untrusted as there's no access list on StarkNet (yet @Avihu).
    ///
    /// # Arguments
    /// * `contract_address` - The StarkNet contract address.
    /// * `entry_point_selector` - The entry point selector.
    /// * `calldata` - The calldata.
    ///
    /// # Returns
    ///
    /// `Ok(Vec<FieldElement>)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_call_contract(
        &self,
        contract_address: FieldElement,
        entry_point_selector: FieldElement,
        calldata: Vec<FieldElement>,
    ) -> Result<Vec<FieldElement>> {
        let opts = FunctionCall {
            contract_address,
            entry_point_selector,
            calldata,
        };

        let last_block = self
            .ethereum_lightclient
            .read()
            .await
            .starknet_last_proven_block()
            .await?
            .as_u64();

        // Call the StarkNet light client.
        self.starknet_lightclient.call(opts, last_block).await
    }

    /// Estimate the fee for a given StarkNet transaction
    /// This function is used to estimate the fee for a given StarkNet transaction.
    ///
    /// # Arguments
    /// * `request` - The broadcasted transaction.
    /// * `block_id` - The block identifier.
    ///
    /// # Returns
    ///
    /// `Ok(FeeEstimate)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_estimate_fee(
        &self,
        request: BroadcastedTransaction,
        block_id: &BlockId,
    ) -> Result<FeeEstimate> {
        // Call the StarkNet light client.
        self.starknet_lightclient
            .estimate_fee(request, block_id)
            .await
    }

    /// Get the nonce at a given address.
    /// This function is used to get the nonce at a given address.
    ///
    /// # Arguments
    ///
    /// * `contract_address` - The StarkNet contract address.
    ///
    /// # Returns
    ///
    /// `Ok(FieldElement)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_get_nonce(&self, address: FieldElement) -> Result<FieldElement> {
        let last_block = self
            .ethereum_lightclient
            .read()
            .await
            .starknet_last_proven_block()
            .await?
            .as_u64();

        self.starknet_lightclient
            .get_nonce(last_block, address)
            .await
    }

    /// Return the timestamp at the time cancelL1ToL2Message was called with a message matching 'msg_hash'.
    /// The function returns 0 if cancelL1ToL2Message was never called.
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// * `msg_hash` - The message hash as bytes32.
    /// # Returns
    /// `Ok(U256)` if the operation was successful - The timestamp at the time cancelL1ToL2Message was called with a message matching 'msg_hash'.
    /// `Ok(U256::zero())` if the operation was successful - The function returns 0 if cancelL1ToL2Message was never called.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_l1_to_l2_message_cancellations(&self, msg_hash: U256) -> Result<U256> {
        // Convert the message hash to bytes32.
        let msg_hash_bytes32 = ethers_helper::u256_to_bytes32_type(msg_hash);
        // Encode the function data.
        let data = ethers_helper::encode_function_data(
            msg_hash_bytes32,
            self.starknet_core_abi.clone(),
            "l1ToL2MessageCancellations",
        )?;
        let data = data.to_vec();

        // Build the call options.
        let call_opts = CallOpts {
            from: None,
            to: self.starknet_core_contract_address,
            gas: None,
            gas_price: None,
            value: None,
            data: Some(data),
        };

        // Call the StarkNet core contract.
        let call_response = self
            .ethereum_lightclient
            .read()
            .await
            .call(&call_opts, BlockTag::Latest)
            .await?;
        Ok(U256::from_big_endian(&call_response))
    }

    /// Return the msg_fee + 1 from the L1ToL2Message hash'. 0 if there is no matching msg_hash
    /// The function returns 0 if L1ToL2Message was never called.
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// * `msg_hash` - The message hash as bytes32.
    /// # Returns
    /// `Ok(U256)` if the operation was successful - The msg_fee + 1 from the L1ToL2Message hash'.
    /// `Ok(U256::zero())` if the operation was successful - The function returns 0 if there is no match on the message hash
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_l1_to_l2_messages(&self, msg_hash: ethers::types::U256) -> Result<U256> {
        // Convert the message hash to bytes32.
        let msg_hash_bytes32 = ethers_helper::u256_to_bytes32_type(msg_hash);
        // Encode the function data.
        let data = ethers_helper::encode_function_data(
            msg_hash_bytes32,
            self.starknet_core_abi.clone(),
            "l1ToL2Messages",
        )?;
        let data = data.to_vec();

        // Build the call options.
        let call_opts = CallOpts {
            from: None,
            to: self.starknet_core_contract_address,
            gas: None,
            gas_price: None,
            value: None,
            data: Some(data),
        };

        // Call the StarkNet core contract.
        let call_response = self
            .ethereum_lightclient
            .read()
            .await
            .call(&call_opts, BlockTag::Latest)
            .await?;
        Ok(U256::from_big_endian(&call_response))
    }

    ///  Returns the msg_fee + 1 for the message with the given 'msgHash', or 0 if no message with such a hash is pending.
    /// The function returns 0 if L2ToL1Message was never called.
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// * `msg_hash` - The message hash as bytes32.
    /// # Returns
    /// `Ok(U256)` if the operation was successful - The msg_fee + 1 from the L2ToL1Message hash'.
    /// `Ok(U256::zero())` if the operation was successful - The function returns 0 if there is no matching message hash
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_l2_to_l1_messages(&self, msg_hash: U256) -> Result<U256> {
        // Convert the message hash to bytes32.
        let msg_hash_bytes32 = ethers_helper::u256_to_bytes32_type(msg_hash);
        // Encode the function data.
        let data = ethers_helper::encode_function_data(
            msg_hash_bytes32,
            self.starknet_core_abi.clone(),
            "l2ToL1Messages",
        )?;
        let data = data.to_vec();

        // Build the call options.
        let call_opts = CallOpts {
            from: None,
            to: self.starknet_core_contract_address,
            gas: None,
            gas_price: None,
            value: None,
            data: Some(data),
        };

        // Call the StarkNet core contract.
        let call_response = self
            .ethereum_lightclient
            .read()
            .await
            .call(&call_opts, BlockTag::Latest)
            .await?;
        Ok(U256::from_big_endian(&call_response))
    }

    /// Return the nonce for the L1ToL2Message bridge.
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// # Returns
    /// `Ok(U256)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_l1_to_l2_message_nonce(&self) -> Result<U256> {
        // Encode the function data.
        let data = ethers_helper::encode_function_data(
            (),
            self.starknet_core_abi.clone(),
            "l1ToL2MessageNonce",
        )?;
        let data = data.to_vec();

        // Build the call options.
        let call_opts = CallOpts {
            from: None,
            to: self.starknet_core_contract_address,
            gas: None,
            gas_price: None,
            value: None,
            data: Some(data),
        };

        // Call the StarkNet core contract.
        let call_response = self
            .ethereum_lightclient
            .read()
            .await
            .call(&call_opts, BlockTag::Latest)
            .await?;
        Ok(U256::from_big_endian(&call_response))
    }

    /// Return block hash and number of latest block.
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// None
    /// # Returns
    /// `Ok(BlockHashAndNumber)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn get_block_hash_and_number(&self) -> Result<BlockHashAndNumber> {
        let cloned_node = self.node.read().await;
        let payload = cloned_node.payload.clone();

        let block = payload.get(&cloned_node.block_number);
        match block {
            Some(block) => Ok(BlockHashAndNumber {
                block_hash: block.block_hash,
                block_number: block.block_number,
            }),
            _ => Err(eyre::eyre!("Block not found")),
        }
    }

    /// Return transaction receipt of a transaction.
    /// # Arguments
    /// * `tx_hash` - The transaction hash as String.
    /// # Returns
    /// `Ok(MaybePendingTransactionReceipt)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn starknet_get_transaction_receipt(
        &self,
        tx_hash: String,
    ) -> Result<MaybePendingTransactionReceipt> {
        let cloned_node = self.node.read().await;
        let state_root = self
            .ethereum_lightclient
            .read()
            .await
            .starknet_state_root()
            .await?
            .to_string();

        if cloned_node.state_root != state_root {
            return Err(eyre::eyre!("State root mismatch"));
        }

        let tx_hash_felt = FieldElement::from_hex_be(&tx_hash).unwrap();
        let tx_receipt = self
            .starknet_lightclient
            .get_transaction_receipt(tx_hash_felt)
            .await?;
        Ok(tx_receipt)
    }
    /// Return block with transaction hashes.
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// BlockId
    /// # Returns
    /// `Ok(MaybePendingBlockWithTxHashes)` if the operation was successful.
    /// `Err(eyre::Report)` if the operation failed.
    pub async fn get_block_with_tx_hashes(
        &self,
        block_id: &BlockId,
    ) -> Result<MaybePendingBlockWithTxHashes> {
        let cloned_node = self.node.read().await;
        let payload = cloned_node.payload.clone();

        let block = match block_id {
            BlockId::Number(block_number) => payload.get(block_number),
            BlockId::Hash(block_hash) => {
                let block = payload
                    .values()
                    .find(|block| block.block_hash == *block_hash);
                match block {
                    Some(block) => Some(block),
                    None => {
                        return Err(eyre::eyre!(
                            "Block with hash {} not found in the payload.",
                            block_hash
                        ))
                    }
                }
            }
            BlockId::Tag(tag) => match tag {
                StarknetBlockTag::Latest => payload.get(&cloned_node.block_number),
                StarknetBlockTag::Pending => {
                    let block = payload
                        .values()
                        .find(|block| block.status == BlockStatus::Pending);
                    match block {
                        Some(block) => Some(block),
                        None => {
                            return Err(eyre::eyre!(
                                "Block with pending status not found in the payload."
                            ))
                        }
                    }
                }
            },
        };

        match block {
            Some(block) => {
                let tx_hashes = block
                    .clone()
                    .transactions
                    .into_iter()
                    .map(|transaction| match transaction {
                        Transaction::Invoke(tx) => match tx {
                            InvokeTransaction::V0(v0_tx) => v0_tx.transaction_hash,
                            InvokeTransaction::V1(v1_tx) => v1_tx.transaction_hash,
                        },
                        Transaction::L1Handler(L1HandlerTransaction {
                            transaction_hash, ..
                        })
                        | Transaction::Declare(DeclareTransaction {
                            transaction_hash, ..
                        })
                        | Transaction::Deploy(DeployTransaction {
                            transaction_hash, ..
                        })
                        | Transaction::DeployAccount(DeployAccountTransaction {
                            transaction_hash,
                            ..
                        }) => transaction_hash,
                    })
                    .collect();
                let block_with_tx_hashes = BlockWithTxHashes {
                    transactions: tx_hashes,
                    status: block.status.clone(),
                    block_hash: block.block_hash,
                    parent_hash: block.parent_hash,
                    block_number: block.block_number,
                    new_root: block.new_root,
                    timestamp: block.timestamp,
                    sequencer_address: block.sequencer_address,
                };
                Ok(MaybePendingBlockWithTxHashes::Block(block_with_tx_hashes))
            }
            _ => Err(eyre::eyre!("Error while retrieving block.")),
        }
    }

    /// Return transaction by inputed hash
    /// See https://github.com/starknet-io/starknet-addresses for the StarkNet core contract address on different networks.
    /// # Arguments
    /// tx_hash: String
    /// # Returns
    /// Transaction
    pub async fn get_transaction_by_hash(&self, tx_hash: String) -> Result<Transaction> {
        let hash = FieldElement::from_str(&tx_hash)?;

        let transaction = self
            .starknet_lightclient
            .get_transaction_by_hash(hash)
            .await
            .unwrap();

        Ok(transaction)
    }
}
