use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use starknet::providers::jsonrpc::models::{BlockId, BlockTag};
use wiremock::{
    matchers::{body_json, method},
    Mock, MockServer, ResponseTemplate,
};

#[derive(Serialize, Debug)]
pub struct StarknetRpcBaseData<'a, StarknetParams> {
    id: usize,
    jsonrpc: &'a str,
    method: &'a str,
    params: StarknetParams,
}

#[derive(Deserialize, Debug)]
pub struct EthJsonRpcResponse<StarknetParams> {
    pub id: usize,
    pub jsonrpc: String,
    pub result: StarknetParams,
}

impl<'a, StarknetParams> StarknetRpcBaseData<'a, StarknetParams> {
    pub const fn stark_block_number(params: StarknetParams) -> Self {
        Self {
            id: 1,
            jsonrpc: "2.0",
            method: "starknet_blockNumber",
            params,
        }
    }

    pub const fn starknet_get_block_transaction_count(params: StarknetParams) -> Self {
        Self {
            id: 1,
            jsonrpc: "2.0",
            method: "starknet_getBlockTransactionCount",
            params,
        }
    }
}

pub async fn setup_wiremock() -> String {
    let mock_server = MockServer::start().await;
    mock_block_number().mount(&mock_server).await;
    mock_get_block_transaction_count().mount(&mock_server).await;
    mock_server.uri()
}

fn mock_block_number() -> Mock {
    Mock::given(method("POST"))
        .and(body_json(StarknetRpcBaseData::stark_block_number(())))
        .respond_with(response_template_with_status(StatusCode::OK).set_body_raw(
            include_str!("data/blocks/starknet_blockNumber.json"),
            "application/json",
        ))
}

fn mock_get_block_transaction_count() -> Mock {
    let latest_block = BlockId::Tag(BlockTag::Latest);
    Mock::given(method("POST"))
        .and(body_json(
            StarknetRpcBaseData::starknet_get_block_transaction_count([&latest_block]),
        ))
        .respond_with(response_template_with_status(StatusCode::OK).set_body_raw(
            include_str!("data/blocks/starknet_getBlockTransactionCount.json"),
            "application/json",
        ))
}

fn response_template_with_status(status_code: StatusCode) -> ResponseTemplate {
    ResponseTemplate::new(status_code)
        .append_header("vary", "Accept-Encoding")
        .append_header("vary", "Origin")
}
