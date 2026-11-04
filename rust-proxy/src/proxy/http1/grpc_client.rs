use crate::vojo::app_error::AppError;
use bytes::Bytes;
use prost_reflect::DescriptorPool;
use prost_reflect::DynamicMessage;
use prost_reflect::MessageDescriptor;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::Response;
use tonic::client::Grpc;
use tonic::codec::Codec;
use tonic::codec::Decoder;
use tonic::codec::Encoder;
use tonic::transport::Channel;
#[derive(Clone)]
pub struct GrpcChanel {
    pub channel: Grpc<Channel>,
    pub descriptor_pool: DescriptorPool,
}
impl GrpcChanel {
    pub async fn do_request(
        &self,
        service_name: String,
        method_name: String,
        body: Bytes,
    ) -> Result<Response<DynamicMessage>, AppError> {
        let service_descriptor = self
            .descriptor_pool
            .get_service_by_name(service_name.as_str())
            .ok_or_else(|| AppError(format!("Service '{service_name}' not found")))?;

        let method_descriptor = service_descriptor
            .methods()
            .find(|x| x.name() == method_name)
            .ok_or_else(|| {
                AppError(format!(
                    "Method '{method_name}' not found in service '{service_name}'"
                ))
            })?;

        let request_descriptor = method_descriptor.input();
        debug!("{request_descriptor:?}");
        let mut deserializer = serde_json::Deserializer::from_slice(&body);

        let dynamic_request = DynamicMessage::deserialize(request_descriptor, &mut deserializer)?;
        let response_descriptor = method_descriptor.output();
        let codec = DynamicCodec {
            response_descriptor,
        };
        let req = tonic::Request::new(dynamic_request);
        let mut channel = self.channel.clone(); // <--- 克隆 channel

        let path = http::uri::PathAndQuery::try_from(format!(
            "/{}/{}",
            service_descriptor.full_name(),
            method_descriptor.name()
        ))?;
        channel.ready().await?;
        let grpc_response = channel.unary(req, path, codec).await?;
        Ok(grpc_response)
    }
}
#[derive(Clone, Default)]
pub struct GrpcClients {
    clients: Arc<HashMap<String, GrpcChanel>>,
}

impl GrpcClients {
    pub async fn new(endpoints: Vec<(String, String)>) -> Result<Self, AppError> {
        let mut clients = HashMap::new();

        for (endpoint_item, proto_descriptor_set) in endpoints {
            let endpoint = Channel::from_shared(endpoint_item.clone())?;
            let channel = endpoint.connect().await?;

            let file_bytes = std::fs::read(&proto_descriptor_set).map_err(|e| {
                AppError(format!(
                    "Failed to read .pb file at '{proto_descriptor_set}': {e}",
                ))
            })?;

            let descriptor_pool = DescriptorPool::decode(file_bytes.as_ref())?;
            clients.insert(
                endpoint_item,
                GrpcChanel {
                    channel: Grpc::new(channel),
                    descriptor_pool,
                },
            );
        }
        Ok(GrpcClients {
            clients: Arc::new(clients),
        })
    }

    pub async fn get_client(&self, endpoint: &str) -> Result<GrpcChanel, AppError> {
        let client = self
            .clients
            .get(endpoint)
            .cloned()
            .ok_or(AppError::from("Can not find chanel."))?;
        Ok(client)
    }
}
#[derive(Debug, Clone)]
pub struct DynamicCodec {
    /// 用于解码响应消息的描述符
    response_descriptor: MessageDescriptor,
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;

    // 编码器很简单，不需要状态
    type Encoder = DynamicEncoder;
    // 解码器需要知道消息的结构，所以我们把描述符传给它
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            descriptor: self.response_descriptor.clone(),
        }
    }
}
use prost_reflect::prost::Message;
use tonic::Status;
use tonic::codec::DecodeBuf;
use tonic::codec::EncodeBuf;
/// DynamicMessage 的编码器
#[derive(Debug, Default, Clone)]
pub struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        // DynamicMessage 已经实现了 prost::Message trait，可以直接使用 encode
        item.encode_raw(buf);
        Ok(())
    }
}

/// DynamicMessage 的解码器
#[derive(Debug, Clone)]
pub struct DynamicDecoder {
    descriptor: MessageDescriptor,
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        // 使用我们持有的描述符来解码
        let item = DynamicMessage::decode(self.descriptor.clone(), buf)
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(item)
    }
}
