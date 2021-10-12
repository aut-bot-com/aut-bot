use anyhow::Context;

fn main() -> anyhow::Result<()> {
    // Compile the feature-gate protobuf definitions into the client code
    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        .compile(&["feature-gate.proto"], &["../../lib/proto"])
        .context("compiling feature-gate.proto definitions")?;

    // Compile the gateway-queue-lib protobuf definitions
    tonic_build::configure()
        .build_client(false)
        .build_server(false)
        .compile(&["event.proto"], &["../gateway-queue-lib/proto"])
        .context("compiling logs/gateway-queue-lib/proto/event.proto definitions")?;

    Ok(())
}
