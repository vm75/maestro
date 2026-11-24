//! Standalone Linux MCP stdio transport. It runs as the workload account and
//! shares the exact HTTP tool dispatcher, state and management job policies.
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use maestro::{
    api::{mcp, tools},
    config::{AppConfig, Platform},
    core::{declarative::CompositeProvider, jobs::Jobs, monitor::Monitor},
    firewall::FirewallManager,
    state::AppState,
    storage::Stores,
};
use tokio::sync::mpsc;

enum Frame {
    Data(Vec<u8>),
    Oversized,
}

fn next_frame(reader: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let mut data = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if data.is_empty() && !oversized {
                return Ok(None);
            }
            return Ok(Some(if oversized {
                Frame::Oversized
            } else {
                Frame::Data(data)
            }));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let len = newline.unwrap_or(available.len());
        if !oversized {
            if data.len() + len > mcp::MAX_MESSAGE {
                oversized = true;
            } else {
                data.extend_from_slice(&available[..len]);
            }
        }
        reader.consume(len + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(if oversized {
                Frame::Oversized
            } else {
                Frame::Data(data)
            }));
        }
    }
}

fn spawn_input() -> mpsc::Receiver<Frame> {
    let (tx, rx) = mpsc::channel(8);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = io::BufReader::new(stdin.lock());
        loop {
            match next_frame(&mut reader) {
                Ok(Some(frame)) => {
                    if tx.blocking_send(frame).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("MCP stdin unavailable: {error}");
                    break;
                }
            }
        }
    });
    rx
}

fn emit(output: &mut impl Write, message: &mcp::JsonRpcResponse) -> Result<()> {
    serde_json::to_writer(&mut *output, message)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2 && args[0] == "--config",
        "usage: maestro-mcp-stdio --config /path/to/maestro.yaml"
    );
    let config_path = PathBuf::from(&args[1]);
    let config = AppConfig::load(&config_path)?;
    ensure!(
        config.platform == Platform::Linux,
        "MCP requires native Linux YAML"
    );
    config.validate()?;
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "MCP stdio must run as the non-root workload account"
    );
    config.ensure_directories()?;
    let stores = Stores::load(&config)
        .await
        .context("could not load Linux state")?;
    let firewall = FirewallManager::new(config.clone(), stores.clone()).await?;
    let state = AppState::new(config, stores, firewall);
    let started_at = Utc::now();
    let monitor = Monitor::new(state.config.clone());
    let jobs = Jobs::new(state.config.paths.data_dir.clone());
    let store = maestro::core::linux_config::store(&state.config)?;
    let provider = Arc::new(CompositeProvider::new(&state.config, state.runner.clone()));
    let ctx = tools::ToolContext {
        state: &state,
        started_at,
        jobs: &jobs,
        monitor: &monitor,
        store: &store,
        provider: &provider,
    };
    let mut input = spawn_input();
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    let mut phase = 0_u8;
    while let Some(frame) = input.recv().await {
        let bytes = match frame {
            Frame::Data(bytes) => bytes,
            Frame::Oversized => {
                emit(
                    &mut output,
                    &mcp::failure(None, -32600, "MCP message is oversized"),
                )?;
                continue;
            }
        };
        let request = match mcp::parse(&bytes) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(problem) => {
                emit(&mut output, &problem)?;
                continue;
            }
        };
        if request.id.is_none() {
            if request.method == "notifications/initialized" && phase == 1 {
                phase = 2;
            }
            // Notifications never receive responses.
            continue;
        }
        let id = request.id.clone();
        if request.method == "initialize" {
            if phase != 0 {
                emit(
                    &mut output,
                    &mcp::failure(id, -32600, "Already initialized"),
                )?;
                continue;
            }
            if let Some(reply) = mcp::dispatch(&ctx, request).await {
                if reply.error.is_none() {
                    phase = 1;
                }
                emit(&mut output, &reply)?;
            }
            continue;
        }
        if phase != 2 && request.method != "ping" {
            emit(
                &mut output,
                &mcp::failure(id, -32000, "Complete initialization first"),
            )?;
            continue;
        }
        if let Some(reply) = mcp::dispatch(&ctx, request).await {
            emit(&mut output, &reply)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdio_frames_are_line_delimited_and_bounded() {
        let mut input = io::Cursor::new(b"first\nsecond\n".to_vec());
        match next_frame(&mut input).unwrap().unwrap() {
            Frame::Data(data) => assert_eq!(data, b"first"),
            Frame::Oversized => panic!("unexpected oversized frame"),
        }
        match next_frame(&mut input).unwrap().unwrap() {
            Frame::Data(data) => assert_eq!(data, b"second"),
            Frame::Oversized => panic!("unexpected oversized frame"),
        }
        assert!(next_frame(&mut input).unwrap().is_none());
        let mut input = io::Cursor::new(vec![b'x'; mcp::MAX_MESSAGE + 1]);
        assert!(matches!(
            next_frame(&mut input).unwrap(),
            Some(Frame::Oversized)
        ));
    }
}
