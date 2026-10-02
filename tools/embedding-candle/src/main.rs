//! Bounded BEM2 process provider for the pinned MiniLM sentence-embedding recipe.
//!
//! The local service supervises this executable, verifies both artifact hashes,
//! and owns the typed execution identity. This process loads one model per
//! bounded batch and writes no response until every vector is validated.

#![forbid(unsafe_code)]

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use models::bert::{BertModel, Config as BertConfig};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use tokenizers::{Tokenizer, TruncationParams};

mod models;

type ProviderResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const MODEL_DIMENSIONS: usize = 384;
const MODEL_VOCABULARY: usize = 30_522;
const MAX_TOKEN_COUNT: usize = 256;
const INFERENCE_MICROBATCH: usize = 16;
const MAX_REQUEST_ITEMS: usize = 256;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 20 * 1024 * 1024;
const MAX_MODEL_BYTES: usize = 200 * 1024 * 1024;
const MAX_TOKENIZER_BYTES: usize = 4 * 1024 * 1024;
const MODEL_ARTIFACT_BLAKE3: &str =
    "8087e9bf97c265f8435ed268733ecf3791825ad24850fd5d84d89e32ee3a589a";
const TOKENIZER_ARTIFACT_BLAKE3: &str =
    "82483bb4f0bdb81779f295ecc5a93285d2156834e994a2169f9800e4c8f250c1";
const REQUEST_HEADER_BYTES: usize = 108;
const ITEM_HEADER_BYTES: usize = 36;
const RESPONSE_HEADER_BYTES: usize = 10;
const RESPONSE_ITEM_HEADER_BYTES: usize = 32;

const BERT_CONFIG: &str = r#"{
  "vocab_size": 30522,
  "hidden_size": 384,
  "num_hidden_layers": 6,
  "num_attention_heads": 12,
  "intermediate_size": 1536,
  "hidden_act": "gelu",
  "hidden_dropout_prob": 0.1,
  "max_position_embeddings": 512,
  "type_vocab_size": 2,
  "initializer_range": 0.02,
  "layer_norm_eps": 1e-12,
  "pad_token_id": 0,
  "model_type": "bert"
}"#;

struct RequestItem {
    identity: [u8; 32],
    text: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("embedding provider failed: {error}");
        std::process::exit(2);
    }
}

fn run() -> ProviderResult<()> {
    let device_name = std::env::var("BACKEND_EMBEDDING_DEVICE")?;
    let device = select_device(&device_name)?;
    let model_path = std::env::var("BACKEND_EMBEDDING_MODEL_FILE")?;
    let tokenizer_path = std::env::var("BACKEND_EMBEDDING_TOKENIZER_FILE")?;
    let model_bytes = read_bounded(&model_path, MAX_MODEL_BYTES)?;
    let tokenizer_bytes = read_bounded(&tokenizer_path, MAX_TOKENIZER_BYTES)?;
    let frame = read_stdin_bounded(MAX_REQUEST_BYTES)?;
    let items = parse_request(&frame, &model_bytes, &tokenizer_bytes)?;

    let tokenizer = Tokenizer::from_bytes(&tokenizer_bytes)?;
    if tokenizer.get_vocab_size(false) != MODEL_VOCABULARY {
        return Err("tokenizer vocabulary does not match MiniLM-L6-H384".into());
    }
    let mut tokenizer = tokenizer;
    // The pinned tokenizer JSON includes export-time fixed padding to 128.
    // Sentence Transformers pads dynamically per batch, so disable that
    // stored padding before the helper builds bounded microbatches itself.
    tokenizer.with_padding(None);
    tokenizer.with_truncation(Some(TruncationParams {
        max_length: MAX_TOKEN_COUNT,
        ..TruncationParams::default()
    }))?;

    let config: BertConfig = serde_json::from_str(BERT_CONFIG)?;
    if config.hidden_size != MODEL_DIMENSIONS || config.vocab_size != MODEL_VOCABULARY {
        return Err("compiled MiniLM architecture contract is invalid".into());
    }
    let variables = VarBuilder::from_buffered_safetensors(model_bytes, DType::F32, &device)?;
    let model = BertModel::load(variables, &config)?;
    let embeddings = infer(&model, &tokenizer, &device, &items)?;
    write_response(&items, &embeddings)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn select_device(name: &str) -> ProviderResult<Device> {
    match name {
        "metal" => Ok(Device::new_metal(0)?),
        "cpu" => select_cpu_device(),
        _ => Err("BACKEND_EMBEDDING_DEVICE must be metal or cpu".into()),
    }
}

#[cfg(not(target_os = "macos"))]
fn select_device(name: &str) -> ProviderResult<Device> {
    match name {
        "cpu" => select_cpu_device(),
        "metal" => Err("Metal is available only on macOS builds".into()),
        _ => Err("BACKEND_EMBEDDING_DEVICE must be cpu".into()),
    }
}

#[cfg(feature = "cpu")]
fn select_cpu_device() -> ProviderResult<Device> {
    Ok(Device::Cpu)
}

#[cfg(not(feature = "cpu"))]
fn select_cpu_device() -> ProviderResult<Device> {
    Err("CPU inference is disabled in this build".into())
}

fn read_bounded(path: &str, limit: usize) -> ProviderResult<Vec<u8>> {
    let file = File::open(path)?;
    let expected = usize::try_from(file.metadata()?.len())?;
    if expected == 0 || expected > limit {
        return Err("embedding artifact exceeds its configured byte bound".into());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(expected)?;
    file.take(u64::try_from(limit)?.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() != expected || bytes.len() > limit {
        return Err("embedding artifact changed while being read".into());
    }
    Ok(bytes)
}

fn read_stdin_bounded(limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(limit.min(1024 * 1024))?;
    io::stdin()
        .lock()
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "embedding request exceeds its byte bound",
        ));
    }
    Ok(bytes)
}

fn parse_request(
    frame: &[u8],
    model_bytes: &[u8],
    tokenizer_bytes: &[u8],
) -> ProviderResult<Vec<RequestItem>> {
    if frame.len() < REQUEST_HEADER_BYTES || &frame[..4] != b"BEM2" {
        return Err("expected a BEM2 request".into());
    }
    if !matches!(frame[4], 1 | 2) || frame[5] != 1 {
        return Err("unsupported embedding purpose or normalization".into());
    }
    let dimensions = usize::from(u16::from_be_bytes([frame[6], frame[7]]));
    if dimensions != MODEL_DIMENSIONS {
        return Err("BEM2 dimension does not match MiniLM-L6-H384".into());
    }
    if frame[40..72] != *blake3::hash(model_bytes).as_bytes()
        || frame[72..104] != *blake3::hash(tokenizer_bytes).as_bytes()
    {
        return Err("BEM2 model or tokenizer identity does not match its artifact".into());
    }
    let count = usize::try_from(u32::from_be_bytes(frame[104..108].try_into()?))?;
    if count == 0 || count > MAX_REQUEST_ITEMS {
        return Err("BEM2 item count exceeds its bound".into());
    }
    let model_identity = hexadecimal(*blake3::hash(model_bytes).as_bytes());
    let tokenizer_identity = hexadecimal(*blake3::hash(tokenizer_bytes).as_bytes());
    if model_identity != MODEL_ARTIFACT_BLAKE3 || tokenizer_identity != TOKENIZER_ARTIFACT_BLAKE3 {
        return Err("artifacts do not match the pinned MiniLM model revision".into());
    }
    let mut items = Vec::new();
    items.try_reserve_exact(count)?;
    let mut identities = BTreeSet::new();
    let mut offset = REQUEST_HEADER_BYTES;
    for _ in 0..count {
        let header_end = offset
            .checked_add(ITEM_HEADER_BYTES)
            .ok_or("request extent overflow")?;
        if header_end > frame.len() {
            return Err("truncated BEM2 item header".into());
        }
        let mut identity = [0_u8; 32];
        identity.copy_from_slice(&frame[offset..offset + 32]);
        let length = usize::try_from(u32::from_be_bytes(
            frame[offset + 32..header_end].try_into()?,
        ))?;
        if !identities.insert(identity) || length == 0 || length > MAX_TEXT_BYTES {
            return Err("duplicate identity or invalid BEM2 text length".into());
        }
        offset = header_end;
        let text_end = offset
            .checked_add(length)
            .ok_or("request extent overflow")?;
        if text_end > frame.len() {
            return Err("truncated BEM2 text".into());
        }
        let text = std::str::from_utf8(&frame[offset..text_end])?.to_owned();
        items.push(RequestItem { identity, text });
        offset = text_end;
    }
    if offset != frame.len() {
        return Err("BEM2 request has trailing bytes".into());
    }
    Ok(items)
}

fn infer(
    model: &BertModel,
    tokenizer: &Tokenizer,
    device: &Device,
    items: &[RequestItem],
) -> ProviderResult<Vec<Vec<f32>>> {
    let mut embeddings = Vec::new();
    embeddings.try_reserve_exact(items.len())?;
    let mut input_ids = Vec::new();
    let mut token_type_ids = Vec::new();
    let mut attention_mask = Vec::new();
    for batch in items.chunks(INFERENCE_MICROBATCH) {
        let encodings = batch
            .iter()
            .map(|item| tokenizer.encode(item.text.as_str(), true))
            .collect::<Result<Vec<_>, _>>()?;
        let sequence_length = encodings
            .iter()
            .map(|encoding| encoding.get_ids().len())
            .max()
            .ok_or("empty inference microbatch")?;
        if sequence_length == 0 || sequence_length > MAX_TOKEN_COUNT {
            return Err("tokenizer output exceeds the 256-token recipe".into());
        }
        let cell_count = batch
            .len()
            .checked_mul(sequence_length)
            .ok_or("token batch extent overflow")?;
        input_ids.clear();
        token_type_ids.clear();
        attention_mask.clear();
        input_ids.try_reserve_exact(cell_count)?;
        token_type_ids.try_reserve_exact(cell_count)?;
        attention_mask.try_reserve_exact(cell_count)?;
        for encoding in &encodings {
            let ids = encoding.get_ids();
            let type_ids = encoding.get_type_ids();
            let mask = encoding.get_attention_mask();
            if ids.is_empty()
                || ids.len() > MAX_TOKEN_COUNT
                || ids.len() != type_ids.len()
                || ids.len() != mask.len()
            {
                return Err("tokenizer produced an invalid MiniLM sequence".into());
            }
            input_ids.extend_from_slice(ids);
            input_ids.resize(input_ids.len() + sequence_length - ids.len(), 0);
            token_type_ids.extend_from_slice(type_ids);
            token_type_ids.resize(token_type_ids.len() + sequence_length - type_ids.len(), 0);
            attention_mask.extend_from_slice(mask);
            attention_mask.resize(attention_mask.len() + sequence_length - mask.len(), 0);
        }
        let ids = Tensor::from_slice(&input_ids, (batch.len(), sequence_length), device)?;
        let types = Tensor::from_slice(&token_type_ids, (batch.len(), sequence_length), device)?;
        let mask = Tensor::from_slice(&attention_mask, (batch.len(), sequence_length), device)?;
        let hidden = model.forward(&ids, &types, Some(&mask))?;
        if hidden.dims() != [batch.len(), sequence_length, MODEL_DIMENSIONS] {
            return Err("BERT produced an invalid batch shape".into());
        }
        // Keep attention-mask pooling and normalization on the selected device;
        // only the final 384-coordinate vectors cross back to the host.
        let mask_f32 = mask.to_dtype(DType::F32)?;
        let expanded_mask = mask_f32.unsqueeze(2)?;
        let pooled_sum = hidden.broadcast_mul(&expanded_mask)?.sum(1)?;
        let token_count = mask_f32.sum_keepdim(1)?;
        let pooled = pooled_sum.broadcast_div(&token_count)?;
        let norm = pooled.sqr()?.sum_keepdim(1)?.sqrt()?;
        let normalized = pooled.broadcast_div(&norm)?;
        let vectors = normalized.to_vec2::<f32>()?;
        if vectors.len() != batch.len()
            || vectors
                .iter()
                .any(|vector| vector.len() != MODEL_DIMENSIONS)
        {
            return Err("pooled BERT vectors do not match the recipe shape".into());
        }
        embeddings.extend(vectors);
    }
    Ok(embeddings)
}

fn write_response(items: &[RequestItem], embeddings: &[Vec<f32>]) -> ProviderResult<()> {
    let response = encode_response(items, embeddings)?;
    io::stdout().lock().write_all(&response)?;
    Ok(())
}

fn encode_response(items: &[RequestItem], embeddings: &[Vec<f32>]) -> ProviderResult<Vec<u8>> {
    if items.len() != embeddings.len() {
        return Err("embedding response count does not match its request".into());
    }
    let coordinate_bytes = MODEL_DIMENSIONS
        .checked_mul(size_of::<f32>())
        .and_then(|bytes| bytes.checked_add(RESPONSE_ITEM_HEADER_BYTES))
        .and_then(|bytes| bytes.checked_mul(items.len()))
        .and_then(|bytes| bytes.checked_add(RESPONSE_HEADER_BYTES))
        .ok_or("embedding response extent overflow")?;
    let mut response = Vec::new();
    response.try_reserve_exact(coordinate_bytes)?;
    response.extend_from_slice(b"BEC2");
    response.extend_from_slice(&u16::try_from(MODEL_DIMENSIONS)?.to_be_bytes());
    response.extend_from_slice(&u32::try_from(items.len())?.to_be_bytes());
    for (item, vector) in items.iter().zip(embeddings) {
        if vector.len() != MODEL_DIMENSIONS || vector.iter().any(|value| !value.is_finite()) {
            return Err("embedding response vector is invalid".into());
        }
        let norm_squared = vector.iter().map(|value| value * value).sum::<f32>();
        if (norm_squared - 1.0).abs() > 0.001 {
            return Err("embedding response is not L2 normalized".into());
        }
        response.extend_from_slice(&item.identity);
        for value in vector {
            response.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(response)
}

fn hexadecimal(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "requires the pinned MiniLM artifacts and an available Metal device"]
    fn pinned_model_cpu_and_metal_match_and_round_trip_bem2() {
        let model_path = std::env::var("BACKEND_EMBEDDING_MODEL_FILE")
            .expect("set the pinned MiniLM model artifact path");
        let tokenizer_path = std::env::var("BACKEND_EMBEDDING_TOKENIZER_FILE")
            .expect("set the pinned MiniLM tokenizer artifact path");
        let model_bytes = read_bounded(&model_path, MAX_MODEL_BYTES).expect("model bytes");
        let tokenizer_bytes =
            read_bounded(&tokenizer_path, MAX_TOKENIZER_BYTES).expect("tokenizer bytes");
        assert_eq!(
            hexadecimal(*blake3::hash(&model_bytes).as_bytes()),
            MODEL_ARTIFACT_BLAKE3
        );
        assert_eq!(
            hexadecimal(*blake3::hash(&tokenizer_bytes).as_bytes()),
            TOKENIZER_ARTIFACT_BLAKE3
        );

        let texts = [
            text_with_content_tokens(6),
            text_with_content_tokens(62),
            text_with_content_tokens(126),
            text_with_content_tokens(254),
        ];
        let identities = [[0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32]];
        let examples = texts
            .iter()
            .zip(identities)
            .map(|(text, identity)| (text.as_str(), identity))
            .collect::<Vec<_>>();
        let frame = request_frame(&examples, &model_bytes, &tokenizer_bytes);
        let items = parse_request(&frame, &model_bytes, &tokenizer_bytes).expect("valid BEM2");
        let mut invalid_frame = frame.clone();
        invalid_frame[40] ^= 1;
        assert!(parse_request(&invalid_frame, &model_bytes, &tokenizer_bytes).is_err());

        let mut tokenizer = Tokenizer::from_bytes(&tokenizer_bytes).expect("tokenizer");
        assert_eq!(tokenizer.get_vocab_size(false), MODEL_VOCABULARY);
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKEN_COUNT,
                ..TruncationParams::default()
            }))
            .expect("tokenizer truncation");
        let sequence_lengths = texts
            .iter()
            .map(|text| {
                tokenizer
                    .encode(text.as_str(), true)
                    .expect("tokenize sample")
                    .get_ids()
                    .len()
            })
            .collect::<Vec<_>>();
        assert_eq!(sequence_lengths, [8, 64, 128, 256]);
        let special = tokenizer
            .encode("a happy person", true)
            .expect("BERT special tokens");
        assert_eq!(
            special.get_tokens().first().map(String::as_str),
            Some("[CLS]")
        );
        assert_eq!(
            special.get_tokens().last().map(String::as_str),
            Some("[SEP]")
        );
        assert_eq!(special.get_attention_mask().iter().sum::<u32>(), 5);
        let truncated = tokenizer
            .encode("word ".repeat(512), true)
            .expect("right truncation at the pinned limit");
        assert_eq!(truncated.get_ids().len(), MAX_TOKEN_COUNT);
        assert_eq!(
            truncated.get_tokens().first().map(String::as_str),
            Some("[CLS]")
        );
        assert_eq!(
            truncated.get_tokens().last().map(String::as_str),
            Some("[SEP]")
        );
        let config: BertConfig = serde_json::from_str(BERT_CONFIG).expect("BERT config");

        let cpu = select_device("cpu").expect("Candle CPU");
        let cpu_load_started = Instant::now();
        let cpu_variables =
            VarBuilder::from_buffered_safetensors(model_bytes.clone(), DType::F32, &cpu)
                .expect("load CPU model weights");
        let cpu_model = BertModel::load(cpu_variables, &config).expect("load CPU BERT");
        let cpu_load_elapsed = cpu_load_started.elapsed();
        let cpu_inference_started = Instant::now();
        let cpu_vectors = infer(&cpu_model, &tokenizer, &cpu, &items).expect("CPU vectors");
        let cpu_cold_elapsed = cpu_inference_started.elapsed();
        let cpu_warm_started = Instant::now();
        let cpu_warm_vectors =
            infer(&cpu_model, &tokenizer, &cpu, &items).expect("warm CPU vectors");
        let cpu_warm_elapsed = cpu_warm_started.elapsed();

        let metal = select_device("metal").expect("Candle Metal");
        let metal_load_started = Instant::now();
        let metal_variables =
            VarBuilder::from_buffered_safetensors(model_bytes, DType::F32, &metal)
                .expect("load Metal model weights");
        let metal_model = BertModel::load(metal_variables, &config).expect("load Metal BERT");
        let metal_load_elapsed = metal_load_started.elapsed();
        let metal_inference_started = Instant::now();
        let metal_vectors = infer(&metal_model, &tokenizer, &metal, &items).expect("Metal vectors");
        let metal_cold_elapsed = metal_inference_started.elapsed();
        let metal_warm_started = Instant::now();
        let metal_warm_vectors =
            infer(&metal_model, &tokenizer, &metal, &items).expect("warm Metal vectors");
        let metal_warm_elapsed = metal_warm_started.elapsed();

        eprintln!(
            "BEM2 benchmark: items={}, token_lengths={sequence_lengths:?}, cpu_load_ms={}, cpu_cold_ms={}, cpu_warm_ms={}, metal_load_ms={}, metal_cold_ms={}, metal_warm_ms={}",
            items.len(),
            cpu_load_elapsed.as_millis(),
            cpu_cold_elapsed.as_millis(),
            cpu_warm_elapsed.as_millis(),
            metal_load_elapsed.as_millis(),
            metal_cold_elapsed.as_millis(),
            metal_warm_elapsed.as_millis(),
        );

        assert_eq!(cpu_vectors.len(), examples.len());
        assert_eq!(metal_vectors.len(), examples.len());
        for (cpu_vector, metal_vector) in cpu_vectors.iter().zip(&metal_vectors) {
            assert_eq!(cpu_vector.len(), MODEL_DIMENSIONS);
            assert_eq!(metal_vector.len(), MODEL_DIMENSIONS);
            let cosine = cpu_vector
                .iter()
                .zip(metal_vector)
                .map(|(cpu, metal)| f64::from(*cpu) * f64::from(*metal))
                .sum::<f64>();
            assert!(cosine >= 0.9999, "CPU/Metal cosine was {cosine}");
        }
        for (cpu_vector, warm_vector) in cpu_vectors.iter().zip(&cpu_warm_vectors) {
            let cosine = cpu_vector
                .iter()
                .zip(warm_vector)
                .map(|(cold, warm)| f64::from(*cold) * f64::from(*warm))
                .sum::<f64>();
            assert!(cosine >= 0.999999, "cold/warm CPU cosine was {cosine}");
        }
        for (metal_vector, warm_vector) in metal_vectors.iter().zip(&metal_warm_vectors) {
            let cosine = metal_vector
                .iter()
                .zip(warm_vector)
                .map(|(cold, warm)| f64::from(*cold) * f64::from(*warm))
                .sum::<f64>();
            assert!(cosine >= 0.999999, "cold/warm Metal cosine was {cosine}");
        }

        let response = encode_response(&items, &metal_vectors).expect("BEC2 response");
        assert_eq!(&response[..4], b"BEC2");
        assert_eq!(
            usize::from(u16::from_be_bytes([response[4], response[5]])),
            MODEL_DIMENSIONS
        );
        assert_eq!(
            usize::try_from(u32::from_be_bytes(
                response[6..10].try_into().expect("response count")
            ))
            .expect("response count fits usize"),
            examples.len()
        );
        for (index, item) in items.iter().enumerate() {
            let start =
                RESPONSE_HEADER_BYTES + index * (RESPONSE_ITEM_HEADER_BYTES + 4 * MODEL_DIMENSIONS);
            assert_eq!(
                &response[start..start + RESPONSE_ITEM_HEADER_BYTES],
                &item.identity
            );
        }
    }

    fn request_frame(
        examples: &[(&str, [u8; 32])],
        model_bytes: &[u8],
        tokenizer_bytes: &[u8],
    ) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"BEM2");
        frame.extend_from_slice(&[2, 1]);
        frame.extend_from_slice(
            &u16::try_from(MODEL_DIMENSIONS)
                .expect("model dimension fits BEM2")
                .to_be_bytes(),
        );
        frame.extend_from_slice(&[0; 32]);
        frame.extend_from_slice(blake3::hash(model_bytes).as_bytes());
        frame.extend_from_slice(blake3::hash(tokenizer_bytes).as_bytes());
        frame.extend_from_slice(
            &u32::try_from(examples.len())
                .expect("example count fits BEM2")
                .to_be_bytes(),
        );
        for (text, identity) in examples {
            frame.extend_from_slice(identity);
            frame.extend_from_slice(
                &u32::try_from(text.len())
                    .expect("example text length fits BEM2")
                    .to_be_bytes(),
            );
            frame.extend_from_slice(text.as_bytes());
        }
        frame
    }

    fn text_with_content_tokens(count: usize) -> String {
        let mut text = "a happy person ".repeat(count / 3);
        match count % 3 {
            0 => {}
            1 => text.push_str("a"),
            _ => text.push_str("a happy"),
        }
        text
    }
}
