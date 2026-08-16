//! Native delivery acknowledgement validation and host-credit forwarding.
//!
//! This is the sole boundary that translates JavaScript's cumulative wire
//! ownership back into the host's cumulative terminal byte/record credit.

use super::*;

impl TerminalClient {
    pub(super) fn acknowledge_delivery(
        &self,
        connection_epoch: u64,
        cumulative_frame_count: u64,
        cumulative_byte_length: u64,
    ) -> Result<Option<HostCharge>, String> {
        let window = self.delivery_window.lock().unwrap().clone();
        let Some(window) = window else {
            return Ok(None);
        };
        match window.acknowledge(
            connection_epoch,
            cumulative_frame_count,
            cumulative_byte_length,
        ) {
            Ok(host) => Ok(host),
            Err(error) => {
                // A same-epoch invalid boundary means native and JavaScript no
                // longer agree on delivery ownership. Do not retry or continue
                // on that ledger: reconnect establishes a fresh epoch/window.
                self.reconnect_transport();
                Err(error)
            }
        }
    }
}

#[tauri::command]
pub fn acknowledge_terminal_delivery(
    client_id: String,
    connection_epoch: u64,
    cumulative_frame_count: u64,
    cumulative_byte_length: u64,
    clients: State<'_, TerminalClients>,
) -> Result<(), String> {
    let client = get_client(&clients, &client_id)?;
    let _serialization = client.delivery_ack_serialization.lock().unwrap();
    let Some(host) = client.acknowledge_delivery(
        connection_epoch,
        cumulative_frame_count,
        cumulative_byte_length,
    )?
    else {
        return Ok(());
    };
    *client.pending_delivery_ack.lock().unwrap() = Some((connection_epoch, host));
    flush_delivery_ack_serialized(&client).inspect_err(|_| client.reconnect_transport())
}

pub(super) fn flush_delivery_ack(client: &TerminalClient) -> Result<(), String> {
    let _serialization = client.delivery_ack_serialization.lock().unwrap();
    flush_delivery_ack_serialized(client)
}

fn flush_delivery_ack_serialized(client: &TerminalClient) -> Result<(), String> {
    let Some((epoch, host)) = *client.pending_delivery_ack.lock().unwrap() else {
        return Ok(());
    };
    let Some(writer) = client.writer.lock().unwrap().clone() else {
        return Ok(());
    };
    writer.write(
        envelope(
            0,
            0,
            Payload::TerminalOutputAck(v1::TerminalOutputAck {
                connection_epoch: epoch,
                cumulative_bytes: host.bytes,
                cumulative_records: host.records,
            }),
        ),
        Instant::now() + REQUEST_TIMEOUT,
    )?;
    let mut pending = client.pending_delivery_ack.lock().unwrap();
    if *pending == Some((epoch, host)) {
        *pending = None;
    }
    Ok(())
}
