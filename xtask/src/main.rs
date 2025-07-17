use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use log;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use tar::Archive;
use which::which;

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Build automation for hyperlight-agents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the complete build and setup process
    Run,
    /// Build guest package only
    BuildGuest,
    /// Build vm-agent binary only
    BuildVmAgent,
    /// Create a base rootfs image (without agent)
    BuildBaseRootfs,
    /// Download kernel binary if missing
    DownloadKernel,
    /// Download firecracker binary if missing
    DownloadFirecracker,
    /// Run host package
    RunHost,
    /// Clean all downloaded and built artifacts
    Clean,
}

// Configuration
const KERNEL_VERSION: &str = "5.10.223";
const KERNEL_URL: &str =
    "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.6/x86_64/vmlinux-5.10.223";

const FIRECRACKER_VERSION: &str = "v1.12.1";
const FIRECRACKER_URL: &str = "https://github.com/firecracker-microvm/firecracker/releases/download/v1.12.1/firecracker-v1.12.1-x86_64.tgz";

struct Paths {
    project_root: PathBuf,
    guest_manifest_path: PathBuf,
    vm_agent_manifest_path: PathBuf,
    vm_images_dir: PathBuf,
    firecracker_dir: PathBuf,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
    firecracker_binary: PathBuf,
}

impl Paths {
    fn new() -> Result<Self> {
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let guest_manifest_path = project_root.join("guest/Cargo.toml");
        let vm_agent_manifest_path = project_root.join("vm-agent/Cargo.toml");
        let vm_images_dir = project_root.join("firecracker");
        let firecracker_dir = vm_images_dir.clone();

        Ok(Self {
            project_root: project_root.clone(),
            guest_manifest_path,
            vm_agent_manifest_path,
            vm_images_dir: vm_images_dir.clone(),
            firecracker_dir: firecracker_dir.clone(),
            kernel_path: vm_images_dir.join(format!("vmlinux-{}", KERNEL_VERSION)),
            rootfs_path: vm_images_dir.join("rootfs.ext4"),
            firecracker_binary: firecracker_dir.join("firecracker"),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new()?;

    match cli.command {
        Commands::Run => run_all(&paths).await,
        Commands::BuildGuest => build_guest(&paths),
        Commands::BuildVmAgent => build_vm_agent(&paths),
        Commands::BuildBaseRootfs => build_base_rootfs(&paths),
        Commands::DownloadKernel => download_kernel(&paths).await,
        Commands::DownloadFirecracker => download_firecracker(&paths).await,
        Commands::RunHost => run_host(&paths),
        Commands::Clean => clean(&paths),
    }
}

async fn run_all(paths: &Paths) -> Result<()> {
    log::info!(
        "{}",
        "🚀 Starting complete build process...".bright_blue().bold()
    );

    check_dependencies()?;

    log::info!("\n{}", "1. Building guest package...".bright_cyan());
    build_guest(paths)?;

    log::info!("\n{}", "2. Building rootfs with vm-agent...".bright_cyan());
    add_agent_to_rootfs(paths)?;

    log::info!("\n{}", "3. Checking kernel binary...".bright_cyan());
    let final_kernel_path = paths.vm_images_dir.join("vmlinux");
    if !final_kernel_path.exists() {
        log::info!("Kernel not found, downloading...");
        download_kernel(paths).await?;
    } else {
        log::info!(
            "{} Kernel binary already exists at {}",
            "✓".bright_green(),
            final_kernel_path.display()
        );
    }

    log::info!("\n{}", "6. Checking firecracker binary...".bright_cyan());
    if !paths.firecracker_binary.exists() {
        log::info!("Firecracker not found, downloading...");
        download_firecracker(paths).await?;
    } else {
        log::info!(
            "{} Firecracker binary already exists at {}",
            "✓".bright_green(),
            paths.firecracker_binary.display()
        );
    }

    log::info!("\n{}", "7. Running host application...".bright_cyan());
    run_host(paths)?;

    Ok(())
}

fn check_dependencies() -> Result<()> {
    let mut missing = Vec::new();

    if which("dd").is_err() {
        missing.push("dd (coreutils)");
    }
    if which("mkfs.ext4").is_err() {
        missing.push("mkfs.ext4 (e2fsprogs)");
    }
    if which("sudo").is_err() {
        missing.push("sudo");
    }

    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()?;
    let installed_targets = String::from_utf8_lossy(&output.stdout);

    let required_targets = ["x86_64-unknown-linux-musl", "x86_64-unknown-none"];
    for target in &required_targets {
        if !installed_targets.contains(target) {
            log::info!(
                "{} Installing required target {}...",
                "⚠".bright_yellow(),
                target
            );
            let status = Command::new("rustup")
                .args(["target", "add", target])
                .status()?;
            if !status.success() {
                return Err(anyhow!("Failed to install target {}", target));
            }
            log::info!("{} Installed target {}", "✓".bright_green(), target);
        }
    }

    if !missing.is_empty() {
        log::info!("\n{} Missing required dependencies:", "✗".bright_red());
        for dep in &missing {
            log::info!("  - {}", dep);
        }
        log::info!("\nOn Ubuntu/Debian:");
        log::info!("  sudo apt update && sudo apt install coreutils e2fsprogs sudo");
        log::info!("\nOn Fedora/RHEL:");
        log::info!("  sudo dnf install coreutils e2fsprogs sudo");
        return Err(anyhow!("Missing dependencies"));
    }

    Ok(())
}

fn build_guest(paths: &Paths) -> Result<()> {
    log::info!(
        "{} Building standalone guest package for x86_64-unknown-none...",
        "📦".bright_blue()
    );

    // hyperlight agents fail if built from another directory
    let output = Command::new("cargo")
        .args([
            "build",
            //"--manifest-path",
            //paths.guest_manifest_path.to_str().unwrap(),
            //"--target",
            //"x86_64-unknown-none",
            //"--release",
        ])
        .current_dir(&paths.project_root.join("guest"))
        .output()?;

    if !output.status.success() {
        return Err(anyhow!(
            "Failed to build guest package:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    log::info!("{} Guest package built successfully", "✓".bright_green());
    Ok(())
}

fn build_vm_agent(paths: &Paths) -> Result<()> {
    log::info!(
        "{} Building standalone vm-agent for x86_64-unknown-linux-musl...",
        "📦".bright_blue()
    );

    let output = Command::new("cargo")
        .args([
            "build",
            "--manifest-path",
            paths.vm_agent_manifest_path.to_str().unwrap(),
            "--target",
            "x86_64-unknown-linux-musl",
            "--release",
        ])
        .current_dir(&paths.project_root)
        .output()?;

    if !output.status.success() {
        return Err(anyhow!(
            "Failed to build vm-agent:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    log::info!("{} vm-agent built successfully", "✓".bright_green());
    Ok(())
}

fn build_base_rootfs(paths: &Paths) -> Result<()> {
    log::info!(
        "{} Building base rootfs image from Dockerfile...",
        "🐳".bright_blue()
    );

    if paths.rootfs_path.exists() {
        log::info!(
            "{} Base rootfs image already exists. Skipping.",
            "✓".bright_green()
        );
        return Ok(());
    }

    // 1. Build the Podman image from Dockerfile.rootfs
    let dockerfile_path = paths.vm_images_dir.join("Dockerfile.rootfs");
    if !dockerfile_path.exists() {
        return Err(anyhow!(
            "Dockerfile.rootfs not found in firecracker directory"
        ));
    }

    let podman_image_tag = "hyperlight-rootfs:latest";
    log::info!("Building Podman image from Dockerfile...");
    let build_output = Command::new("podman")
        .args([
            "build",
            "-t",
            podman_image_tag,
            "-f",
            dockerfile_path.to_str().unwrap(),
            paths.vm_images_dir.to_str().unwrap(),
        ])
        .output()?;
    if !build_output.status.success() {
        return Err(anyhow!(
            "Failed to build Podman image for rootfs:\n{}",
            String::from_utf8_lossy(&build_output.stderr)
        ));
    }

    // 2. Create a container from the image (but don't run it)
    let container_name = "hyperlight-rootfs-builder";
    let _ = Command::new("podman").args(["rm", container_name]).output(); // Clean up old container

    log::info!("Creating container from image...");
    let create_output = Command::new("podman")
        .args(["create", "--name", container_name, podman_image_tag])
        .output()?;
    if !create_output.status.success() {
        return Err(anyhow!(
            "Failed to create Podman container:\n{}",
            String::from_utf8_lossy(&create_output.stderr)
        ));
    }

    // 3. Create and format the rootfs.ext4 file (increased size for git, rust, etc.)
    let rootfs_size_mb = 2000;
    log::info!("Creating empty {}MB file for rootfs...", rootfs_size_mb);
    let dd_output = Command::new("dd")
        .args([
            "if=/dev/zero",
            &format!("of={}", paths.rootfs_path.display()),
            "bs=1M",
            &format!("count={}", rootfs_size_mb),
        ])
        .output()?;
    if !dd_output.status.success() {
        let _ = Command::new("podman").args(["rm", container_name]).output();
        return Err(anyhow!(
            "dd command failed:\n{}",
            String::from_utf8_lossy(&dd_output.stderr)
        ));
    }

    log::info!("Formatting file as ext4...");
    let mkfs_output = Command::new("mkfs.ext4")
        .args(["-F", paths.rootfs_path.to_str().unwrap()])
        .output()?;
    if !mkfs_output.status.success() {
        let _ = Command::new("podman").args(["rm", container_name]).output();
        return Err(anyhow!(
            "mkfs.ext4 command failed:\n{}",
            String::from_utf8_lossy(&mkfs_output.stderr)
        ));
    }

    // 4. Mount the rootfs and extract the container filesystem into it
    let temp_dir = tempfile::tempdir()?;
    let mount_point = temp_dir.path().join("mnt");
    fs::create_dir_all(&mount_point)?;

    log::info!("Mounting rootfs image (requires sudo)...");
    let mount_cmd = Command::new("sudo")
        .args([
            "mount",
            "-o",
            "loop",
            paths.rootfs_path.to_str().unwrap(),
            mount_point.to_str().unwrap(),
        ])
        .output()?;
    if !mount_cmd.status.success() {
        let _ = Command::new("podman").args(["rm", container_name]).output();
        return Err(anyhow!(
            "Failed to mount base rootfs:\n{}",
            String::from_utf8_lossy(&mount_cmd.stderr)
        ));
    }

    log::info!("Exporting Podman filesystem and extracting to rootfs (requires sudo)...");
    let export_output = Command::new("podman")
        .args(["export", container_name])
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let tar_extract = Command::new("sudo")
        .args(["tar", "-x", "-C", mount_point.to_str().unwrap()])
        .stdin(export_output.stdout.unwrap())
        .output()?;

    if !tar_extract.status.success() {
        let _ = Command::new("sudo")
            .args(["umount", mount_point.to_str().unwrap()])
            .output();
        let _ = Command::new("podman").args(["rm", container_name]).output();
        return Err(anyhow!(
            "Failed to extract Podman filesystem to rootfs:\n{}",
            String::from_utf8_lossy(&tar_extract.stderr)
        ));
    }

    // 5. Unmount and cleanup
    log::info!("Unmounting rootfs image (requires sudo)...");
    let umount_cmd = Command::new("sudo")
        .args(["umount", mount_point.to_str().unwrap()])
        .output()?;
    if !umount_cmd.status.success() {
        let _ = Command::new("podman").args(["rm", container_name]).output();
        return Err(anyhow!(
            "Failed to unmount base rootfs:\n{}",
            String::from_utf8_lossy(&umount_cmd.stderr)
        ));
    }

    // Clean up the container
    let _ = Command::new("podman").args(["rm", container_name]).output();

    log::info!(
        "{} Base rootfs image with development tools created successfully.",
        "✓".bright_green()
    );
    Ok(())
}

fn add_agent_to_rootfs(paths: &Paths) -> Result<()> {
    log::info!("{} Building rootfs with vm-agent...", "📦".bright_blue());

    // Ensure vm-agent is built
    build_vm_agent(paths)?;

    let vm_agent_src = paths
        .project_root
        .join("vm-agent/target/x86_64-unknown-linux-musl/release/vm-agent");
    if !vm_agent_src.exists() {
        return Err(anyhow!(
            "vm-agent binary not found. Please run `build-vm-agent` first."
        ));
    }

    // Copy vm-agent to firecracker directory for Docker build
    let vm_agent_dest = paths.firecracker_dir.join("vm-agent");
    log::info!("Copying vm-agent binary to firecracker directory...");
    fs::copy(&vm_agent_src, &vm_agent_dest)?;

    // Build the Docker image with the vm-agent
    log::info!("Building Docker image with vm-agent...");
    let docker_build = Command::new("docker")
        .args([
            "build",
            "-f",
            "Dockerfile.rootfs",
            "-t",
            "hyperlight-rootfs",
            ".",
        ])
        .current_dir(&paths.firecracker_dir)
        .output()?;

    if !docker_build.status.success() {
        return Err(anyhow!(
            "Docker build failed:\n{}",
            String::from_utf8_lossy(&docker_build.stderr)
        ));
    }

    // Create a container and export it as a tar
    log::info!("Creating container from image...");
    let create_container = Command::new("docker")
        .args([
            "create",
            "--name",
            "hyperlight-rootfs-temp",
            "hyperlight-rootfs",
        ])
        .current_dir(&paths.firecracker_dir)
        .output()?;

    if !create_container.status.success() {
        return Err(anyhow!(
            "Failed to create container:\n{}",
            String::from_utf8_lossy(&create_container.stderr)
        ));
    }

    // Export the container filesystem to a tar file
    log::info!("Exporting container filesystem...");
    let export_container = Command::new("docker")
        .args(["export", "hyperlight-rootfs-temp"])
        .current_dir(&paths.firecracker_dir)
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    // Create the ext4 filesystem
    log::info!("Creating ext4 filesystem...");
    let dd_cmd = Command::new("dd")
        .args(["if=/dev/zero", "of=rootfs.ext4", "bs=1M", "count=1024"])
        .current_dir(&paths.firecracker_dir)
        .output()?;

    if !dd_cmd.status.success() {
        return Err(anyhow!(
            "Failed to create ext4 file:\n{}",
            String::from_utf8_lossy(&dd_cmd.stderr)
        ));
    }

    // Format as ext4
    let mkfs_cmd = Command::new("mkfs.ext4")
        .args(["-F", "rootfs.ext4"])
        .current_dir(&paths.firecracker_dir)
        .output()?;

    if !mkfs_cmd.status.success() {
        return Err(anyhow!(
            "Failed to format ext4:\n{}",
            String::from_utf8_lossy(&mkfs_cmd.stderr)
        ));
    }

    // Mount the ext4 filesystem
    let temp_dir = tempfile::tempdir()?;
    let mount_point = temp_dir.path().join("mnt");
    fs::create_dir_all(&mount_point)?;

    log::info!("Mounting ext4 filesystem (requires sudo)...");
    let mount_cmd = Command::new("sudo")
        .args([
            "mount",
            "-o",
            "loop",
            "rootfs.ext4",
            mount_point.to_str().unwrap(),
        ])
        .current_dir(&paths.firecracker_dir)
        .output()?;

    if !mount_cmd.status.success() {
        return Err(anyhow!(
            "Failed to mount rootfs:\n{}",
            String::from_utf8_lossy(&mount_cmd.stderr)
        ));
    }

    // Extract the container tar into the mounted filesystem
    log::info!("Extracting container contents (requires sudo)...");
    let mut export_child = export_container;
    let extract_cmd = Command::new("sudo")
        .args(["tar", "-xf", "-", "-C", mount_point.to_str().unwrap()])
        .stdin(export_child.stdout.take().unwrap())
        .output()?;

    if !extract_cmd.status.success() {
        let _ = Command::new("sudo")
            .args(["umount", mount_point.to_str().unwrap()])
            .output();
        return Err(anyhow!(
            "Failed to extract container contents:\n{}",
            String::from_utf8_lossy(&extract_cmd.stderr)
        ));
    }

    thread::sleep(std::time::Duration::from_millis(1000));
    // Unmount the filesystem
    log::info!("Unmounting filesystem...");
    let umount_cmd = Command::new("sudo")
        .args(["umount", mount_point.to_str().unwrap()])
        .output()?;

    if !umount_cmd.status.success() {
        return Err(anyhow!(
            "Failed to unmount rootfs:\n{}",
            String::from_utf8_lossy(&umount_cmd.stderr)
        ));
    }

    // Clean up the temporary container
    let _ = Command::new("docker")
        .args(["rm", "hyperlight-rootfs-temp"])
        .output();

    // Clean up the temporary vm-agent file
    let _ = fs::remove_file(&vm_agent_dest);

    log::info!(
        "{} Rootfs with vm-agent successfully created.",
        "✓".bright_green()
    );
    Ok(())
}

async fn download_kernel(paths: &Paths) -> Result<()> {
    log::info!("Downloading kernel binary...");
    fs::create_dir_all(&paths.vm_images_dir)?;
    let response = reqwest::get(KERNEL_URL).await?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download kernel: HTTP {}",
            response.status()
        ));
    }
    let mut file = File::create(&paths.kernel_path)?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&paths.kernel_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&paths.kernel_path, perms)?;
    }

    // Rename to vmlinux (without version suffix)
    let final_kernel_path = paths.vm_images_dir.join("vmlinux");
    fs::rename(&paths.kernel_path, &final_kernel_path)?;

    log::info!(
        "{} Kernel downloaded and renamed to {}",
        "✓".bright_green(),
        final_kernel_path.display()
    );
    Ok(())
}

async fn download_firecracker(paths: &Paths) -> Result<()> {
    log::info!("Downloading Firecracker binary...");
    fs::create_dir_all(&paths.firecracker_dir)?;
    let response = reqwest::get(FIRECRACKER_URL).await?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download Firecracker: HTTP {}",
            response.status()
        ));
    }
    let temp_file = paths.firecracker_dir.join("firecracker.tgz");
    let mut file = File::create(&temp_file)?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?)?;
    }
    drop(file);

    // Extract to temporary directory
    let temp_extract_dir = paths.firecracker_dir.join("temp_extract");
    fs::create_dir_all(&temp_extract_dir)?;

    let tar_gz = File::open(&temp_file)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(&temp_extract_dir)?;
    fs::remove_file(&temp_file)?;

    // Find the firecracker binary in the extracted directory
    let extracted_binary_path = temp_extract_dir
        .join(format!("release-{}-x86_64", FIRECRACKER_VERSION))
        .join(format!("firecracker-{}-x86_64", FIRECRACKER_VERSION));

    // Copy the binary to the final location
    fs::copy(&extracted_binary_path, &paths.firecracker_binary)?;

    // Clean up temporary directory
    fs::remove_dir_all(&temp_extract_dir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&paths.firecracker_binary)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&paths.firecracker_binary, perms)?;
    }
    log::info!(
        "{} Firecracker downloaded and extracted to {}",
        "✓".bright_green(),
        paths.firecracker_binary.display()
    );
    Ok(())
}

fn run_host(paths: &Paths) -> Result<()> {
    log::info!("\n{}", "Running host application...".bright_green().bold());
    let status = Command::new("cargo")
        .args(["run", "-p", "hyperlight-agents-host"])
        .current_dir(&paths.project_root)
        .env("RUST_LOG", "debug")
        .status()?;
    if !status.success() {
        return Err(anyhow!("Host application exited with error"));
    }
    Ok(())
}

fn clean(paths: &Paths) -> Result<()> {
    log::info!(
        "{}",
        "Cleaning downloaded and built artifacts...".bright_blue()
    );
    if paths.kernel_path.exists() {
        fs::remove_file(&paths.kernel_path)?;
        log::info!(
            "{} Removed kernel: {}",
            "✓".bright_green(),
            paths.kernel_path.display()
        );
    }
    if paths.rootfs_path.exists() {
        fs::remove_file(&paths.rootfs_path)?;
        log::info!(
            "{} Removed rootfs: {}",
            "✓".bright_green(),
            paths.rootfs_path.display()
        );
    }
    if paths.firecracker_dir.exists() {
        fs::remove_dir_all(&paths.firecracker_dir)?;
        log::info!(
            "{} Removed firecracker: {}",
            "✓".bright_green(),
            paths.firecracker_dir.display()
        );
    }
    let output = Command::new("cargo")
        .args(["clean"])
        .current_dir(&paths.project_root)
        .output()?;
    if output.status.success() {
        log::info!("{} Cleaned cargo build artifacts", "✓".bright_green());
    }
    log::info!("{}", "✓ Cleanup complete".bright_green());
    Ok(())
}
