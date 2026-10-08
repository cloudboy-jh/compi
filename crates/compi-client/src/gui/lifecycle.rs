use compi_protocol::ErrorCode;

#[derive(Clone, Debug)]
pub(super) struct PaneError {
    pub code: Option<ErrorCode>,
    pub message: String,
}

impl PaneError {
    pub fn from_error(error: &(dyn std::error::Error + Send + Sync + 'static)) -> Self {
        Self {
            code: error
                .downcast_ref::<compi_protocol::DaemonError>()
                .map(|error| error.code),
            message: error.to_string(),
        }
    }
}
