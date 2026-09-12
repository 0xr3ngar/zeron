//! Exercise the same device-local installer used by Settings → Harnesses.
use zeron_harness::installations::{InstallAction, apply, inspect};
use zeron_proto::HarnessId;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let harness: HarnessId = serde_json::from_value(args.next().ok_or("harness required")?.into())?;
    let result = match args.next().as_deref() {
        Some("install") => apply(harness, InstallAction::Install).await?,
        Some("rollback") => apply(harness, InstallAction::Rollback).await?,
        Some("existing") => {
            apply(
                harness,
                InstallAction::UseExisting {
                    path: args.next().map(Into::into),
                },
            )
            .await?
        }
        _ => inspect(harness).await,
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
