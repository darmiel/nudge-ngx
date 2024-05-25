use std::borrow::Cow;
use std::fs::OpenOptions;
use std::io::{Write};
use std::net::{Ipv4Addr, UdpSocket};

use clap::Parser;
use console::style;
use dialoguer::Confirm;
use dialoguer::theme::ColorfulTheme;
use humansize::{DECIMAL, format_size};
use indicatif::ProgressBar;

use crate::error::NudgeError;
use crate::models::{FileInfo, R2XRequestSenderConnectionMessage, R2XRequestFileInfoMessage};
use crate::passphrase::Passphrase;
use crate::reliable_udp::ReliableUdpSocket;
use crate::utils::{current_unix_millis, hide_or_get_hostname, init_socket, new_downloader_progressbar, question_theme, receive_and_parse_and_expect, serialize_and_send};
use crate::utils::{DEFAULT_RELAY_HOST, DEFAULT_RELAY_PORT, DEFAULT_CHUNK_SIZE};

#[derive(Parser, Debug)]
pub struct Get {
    passphrase: String,

    #[clap(short = 'o', long)]
    out_file: String,

    #[clap(short = 'x', long, default_value = DEFAULT_RELAY_HOST)]
    relay_host: String,

    #[clap(short = 'y', long, default_value = DEFAULT_RELAY_PORT)]
    relay_port: u16,

    #[clap(short, long, default_value = "500")]
    delay: u64,

    #[clap(short, long, default_value = DEFAULT_CHUNK_SIZE)]
    chunk_size: u32,

    #[clap(short, long, default_value = "false")]
    force: bool,

    /// If enabled, won't send the hostname to the sender
    #[clap(long, default_value = "false")]
    hide_hostname: bool,
}


impl Get {
    pub fn run(&self) -> Result<(), NudgeError> {
        let local_bind_address = (Ipv4Addr::from(0u32), 0);
        debug!("Binding UDP socket to local address: {:?}", local_bind_address);
        let socket = UdpSocket::bind(local_bind_address)?;

        let relay_address = format!("{}:{}", self.relay_host, self.relay_port);
        debug!("Connecting to relay-server: {}...", relay_address);
        socket.connect(relay_address)?;

        // RECV_REQ
        let passphrase = Passphrase(Cow::Owned(self.passphrase.clone()));
        debug!("Sending R2XRequestFileInfoMessage with passphrase: {}...", passphrase.0);
        serialize_and_send(&socket, "R2X_RFI", &R2XRequestFileInfoMessage {
            passphrase: passphrase.clone(),
        })?;

        debug!("Waiting for FileInfo...");
        let recv_ack: FileInfo = receive_and_parse_and_expect(&socket, "X2R_AFI")?;
        debug!("Received FileInfo: {:?}", recv_ack);

        // display file information
        println!(
            "{} Meta: {} by {} [{}]",
            style("[✔]").bold().green(),
            style(&recv_ack.file_name).yellow(),
            style(&recv_ack.sender_host).cyan(),
            format_size(recv_ack.file_size, DECIMAL)
        );

        // ask if we really want to download the file
        if !self.force && !Confirm::with_theme(&question_theme())
            .with_prompt("Do you want to download the file?")
            .interact()
            .unwrap()
        {
            println!("Cancelled by user.");
            return Ok(());
        }

        // Opening the file for writing, creating it if it doesn't exist
        let mut file = OpenOptions::new()
            .truncate(false)
            .write(true)
            .create(true)
            .open(&self.out_file)?;
        file.set_len(recv_ack.file_size)?;

        // ask the sender to connect to us
        let hostname = hide_or_get_hostname(self.hide_hostname)?;
        debug!(
            "Requesting sender to connect to us ({}) via R2XRequestSenderConnectionMessage...",
            hostname
        );
        serialize_and_send(&socket, "R2X_RSC", &R2XRequestSenderConnectionMessage {
            passphrase,
            file_hash: recv_ack.file_hash,
            receiver_host: hostname,
        })?;

        println!(
            "{} Connecting to {} ({})...",
            style("[~]").bold().yellow(),
            style(&recv_ack.sender_host).cyan(),
            style(&recv_ack.sender_addr).dim()
        );
        socket.connect(recv_ack.sender_addr)?;

        debug!("Initializing socket connection...");
        init_socket(&socket)?;
        debug!("Ready to receive data!");

        // wrap the socket in a "reliable udp socket"
        let mut safe_connection = ReliableUdpSocket::new(socket);

        println!(
            "{} Receiving {} (chunk-size: {})...",
            style("[~]").bold().yellow(),
            format_size(recv_ack.file_size, DECIMAL),
            style(format_size(self.chunk_size, DECIMAL)).dim()
        );

        let progress_bar = new_downloader_progressbar(recv_ack.file_size);

        // Used for calculating the total time taken
        let start_time = current_unix_millis();

        // Used for updating the progressbar
        let mut bytes_received: u64 = 0;

        // update progress every 25 KiB
        let update_progress_rate = (1024 * 25) / self.chunk_size;
        let mut current_progress = 0;

        let mut buffer: Vec<u8> = vec![0; self.chunk_size as usize];

        loop {
            let (read_buffer, bytes_read) = safe_connection.read(&mut buffer)?;
            if bytes_read == 0 {
                progress_bar.finish_with_message("Transfer complete! 🎉");
                break;
            }

            let buffer = &read_buffer[..bytes_read];
            file.write_all(buffer)?;
            file.flush()?;

            bytes_received += bytes_read as u64;

            current_progress += 1;
            if current_progress % update_progress_rate == 0 {
                progress_bar.set_position(bytes_received);
            }
        }

        println!(
            "{} File received successfully in {}s!",
            style("[✔]").bold().green(),
            (current_unix_millis() - start_time) as f64 / 1000.0
        );
        Ok(())
    }
}