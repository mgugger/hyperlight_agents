use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() {
    match UnixStream::connect("/tmp/vsock-test-vm.sock") {
        Ok(mut stream) => {
            println!("Connected to VSOCK socket");
            
            // Send the required Firecracker VSOCK handshake
            let handshake = "CONNECT 1234\n";
            if let Err(e) = stream.write_all(handshake.as_bytes()) {
                println!("Failed to write handshake: {}", e);
                return;
            }
            println!("Handshake sent: {}", handshake.trim());
            
            // Wait for handshake response
            let mut handshake_buffer = [0; 256];
            match stream.read(&mut handshake_buffer) {
                Ok(n) => {
                    let handshake_response = String::from_utf8_lossy(&handshake_buffer[..n]);
                    println!("Handshake response: {}", handshake_response.trim());
                }
                Err(e) => {
                    println!("Failed to read handshake response: {}", e);
                    return;
                }
            }
            
            // Now send the JSON command
            let command = r#"{"command":"ls /"}"#;
            
            if let Err(e) = stream.write_all(command.as_bytes()) {
                println!("Failed to write command: {}", e);
                return;
            }
            
            println!("Command sent: {}", command);
            
            let mut buffer = [0; 1024];
            match stream.read(&mut buffer) {
                Ok(n) => {
                    let response = String::from_utf8_lossy(&buffer[..n]);
                    println!("Response: {}", response);
                }
                Err(e) => println!("Failed to read: {}", e)
            }
        }
        Err(e) => println!("Failed to connect: {}", e)
    }
}