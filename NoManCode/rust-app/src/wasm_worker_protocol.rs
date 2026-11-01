//! Bounded single-request worker protocol. Read deadlines are the caller's responsibility.
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const VERSION: u32 = 1;
pub const MAX_MANIFEST: usize = 4096;
pub const MAX_INPUT: usize = 8192;
pub const MAX_MODULE: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE: usize = 512 * 1024;
pub const MAX_TOOLS: usize = 256;
pub const MAX_TOTAL: usize = MAX_MANIFEST + MAX_INPUT + MAX_MODULE + 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub request_id: String,
    pub nonce: String,
    pub binding_generation: u64,
    pub plugin_id: String,
    pub plugin_version: String,
    pub digest: String,
    pub fuel: u64,
    pub memory_pages: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Read,
    List,
    Search,
    Write,
    Command,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum Phase {
    Before { tools: Vec<ToolClass> },
    AfterNoTools,
    AfterTools { tools: Vec<ToolClass> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub round: u32,
    pub max_rounds: u32,
    pub decision: Phase,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vote {
    ApplyHostPolicy,
    Deny,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    UseTools { indices: Vec<usize> },
    ToolVotes { votes: Vec<Vote> },
    Finish,
    Stop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    Protocol,
    Identity,
    Manifest,
    Module,
    Execution,
    Decision,
    Internal,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Ok { decision: Decision },
    Error { error: Error },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub manifest: Manifest,
    pub outcome: Outcome,
}
pub struct Request {
    pub manifest: Manifest,
    pub input: Input,
    pub module: Vec<u8>,
}

fn token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn version_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
impl Manifest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != VERSION
            || !token(&self.request_id, 64)
            || self.nonce.len() != 32
            || !self.nonce.bytes().all(|b| b.is_ascii_hexdigit())
            || !token(&self.plugin_id, 64)
            || !version_token(&self.plugin_version)
            || self.digest.len() != 64
            || !self.digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !(1..=50_000_000).contains(&self.fuel)
            || !(1..=64).contains(&self.memory_pages)
        {
            return Err(Error::Manifest);
        }
        Ok(())
    }
}
impl Input {
    pub fn validate(&self) -> Result<(), Error> {
        let count = match &self.decision {
            Phase::Before { tools } | Phase::AfterTools { tools } => tools.len(),
            Phase::AfterNoTools => 0,
        };
        if self.max_rounds == 0
            || self.max_rounds > 10_000
            || self.round >= self.max_rounds
            || count > MAX_TOOLS
            || matches!(&self.decision, Phase::AfterTools {tools} if tools.is_empty())
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }
    pub fn validate_decision(&self, decision: &Decision) -> Result<(), Error> {
        let valid = match (&self.decision, decision) {
            (_, Decision::Stop) => true,
            (Phase::Before { tools }, Decision::UseTools { indices }) => {
                indices.iter().all(|i| *i < tools.len()) && indices.windows(2).all(|w| w[0] < w[1])
            }
            (Phase::AfterNoTools, Decision::Finish) => true,
            (Phase::AfterTools { tools }, Decision::ToolVotes { votes }) => {
                votes.len() == tools.len()
            }
            _ => false,
        };
        if valid { Ok(()) } else { Err(Error::Decision) }
    }
}
fn frame<R: Read>(reader: &mut R, max: usize) -> Result<Vec<u8>, Error> {
    let mut len = [0; 4];
    reader.read_exact(&mut len).map_err(|_| Error::Protocol)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > max {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).map_err(|_| Error::Protocol)?;
    Ok(bytes)
}
fn write_frame<W: Write>(writer: &mut W, bytes: &[u8], max: usize) -> Result<(), Error> {
    if bytes.is_empty() || bytes.len() > max {
        return Err(Error::Protocol);
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|_| writer.write_all(bytes))
        .map_err(|_| Error::Protocol)
}
fn eof<R: Read>(reader: &mut R) -> Result<(), Error> {
    let mut extra = [0; 1];
    if reader.read(&mut extra).map_err(|_| Error::Protocol)? != 0 {
        return Err(Error::Protocol);
    }
    Ok(())
}
pub fn read_request<R: Read>(reader: &mut R) -> Result<Request, Error> {
    let manifest: Manifest =
        serde_json::from_slice(&frame(reader, MAX_MANIFEST)?).map_err(|_| Error::Protocol)?;
    manifest.validate()?;
    let input: Input =
        serde_json::from_slice(&frame(reader, MAX_INPUT)?).map_err(|_| Error::Protocol)?;
    input.validate()?;
    let module = frame(reader, MAX_MODULE)?;
    eof(reader)?;
    Ok(Request {
        manifest,
        input,
        module,
    })
}
pub fn write_request<W: Write>(writer: &mut W, request: &Request) -> Result<(), Error> {
    request.manifest.validate()?;
    request.input.validate()?;
    write_frame(
        writer,
        &serde_json::to_vec(&request.manifest).map_err(|_| Error::Protocol)?,
        MAX_MANIFEST,
    )?;
    write_frame(
        writer,
        &serde_json::to_vec(&request.input).map_err(|_| Error::Protocol)?,
        MAX_INPUT,
    )?;
    write_frame(writer, &request.module, MAX_MODULE)
}
pub fn write_response<W: Write>(writer: &mut W, response: &Response) -> Result<(), Error> {
    write_frame(
        writer,
        &serde_json::to_vec(response).map_err(|_| Error::Protocol)?,
        MAX_RESPONSE,
    )
}
pub fn read_response<R: Read>(
    reader: &mut R,
    expected: &Manifest,
    input: &Input,
) -> Result<Response, Error> {
    let response: Response =
        serde_json::from_slice(&frame(reader, MAX_RESPONSE)?).map_err(|_| Error::Protocol)?;
    eof(reader)?;
    if &response.manifest != expected {
        return Err(Error::Identity);
    }
    if let Outcome::Ok { decision } = &response.outcome {
        input.validate_decision(decision)?;
    }
    Ok(response)
}
