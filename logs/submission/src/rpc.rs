//! Contains generated code from the shared service Protobuf RPC definitions

pub mod logs {
    // Ignore clippy linting on generated code
    #[allow(clippy::all, clippy::pedantic, clippy::nursery)]
    pub mod event {
        tonic::include_proto!("logs.event");
    }

    // Ignore clippy linting on generated code
    #[allow(clippy::all, clippy::pedantic, clippy::nursery)]
    pub mod submission {
        tonic::include_proto!("logs.submission");
    }
}

// Ignore clippy linting on generated code
#[allow(clippy::all, clippy::pedantic, clippy::nursery)]
pub mod logs_submission_schema {
    tonic::include_proto!("logs_submission_schema");
}
