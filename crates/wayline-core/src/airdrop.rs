//! Application values converted from Owly's validated protocol messages.
use owly_airdrop::proto::messages::{AskRequest, Offer};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingFile {
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingTransfer {
    pub id: String,
    pub sender_name: String,
    pub sender_model: Option<String>,
    pub files: Vec<IncomingFile>,
    pub urls: Vec<String>,
    pub icon: Option<Vec<u8>>,
    pub sender_record_data: Option<Vec<u8>>,
}
impl IncomingTransfer {
    pub(crate) fn from_ask(ask: &AskRequest) -> Self {
        let (files, urls) = match &ask.offer {
            Offer::Files(files) => (
                files
                    .iter()
                    .map(|f| IncomingFile {
                        name: f.name.clone(),
                        kind: f.file_type.clone(),
                        size: f.size,
                    })
                    .collect(),
                Vec::new(),
            ),
            Offer::Urls(urls) => (Vec::new(), urls.clone()),
        };
        Self {
            id: ask.transfer_id.clone().unwrap_or_else(|| {
                format!(
                    "incoming-{}",
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                )
            }),
            sender_name: ask.sender_computer_name.clone(),
            sender_model: (!ask.sender_model_name.is_empty())
                .then(|| ask.sender_model_name.clone()),
            files,
            urls,
            icon: ask.file_icon.clone(),
            sender_record_data: ask.sender_record.clone(),
        }
    }
}
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
