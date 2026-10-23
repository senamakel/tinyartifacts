use std::path::PathBuf;

use tiny_artifacts::{ArtifactKind, ArtifactRecord, ArtifactStore, FsArtifactStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let directory = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    let record = ArtifactRecord::new(
        "previous-release-1",
        "Previous release record",
        ArtifactKind::Markdown,
        "# Preserved artifact\n\nWritten by the 0.1.0 filesystem store.",
        "legacy-agent",
        1_704_067_200_000,
    );
    FsArtifactStore::new(directory).upsert(&record).await?;
    Ok(())
}
