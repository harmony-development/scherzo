use harmony_build::{Builder, Protocol, Result};

fn main() -> Result<()> {
    Ok(())
}

fn build_protocol(
    version: &str,
    stable_svcs: &[&str],
    staging_svcs: &[&str],
    perms: bool,
) -> Result<()> {
    let protocol_path = format!("protocols/{}", version);
    let out_dir = {
        let mut dir = std::env::var("OUT_DIR").expect("no out dir, how");
        dir.push('/');
        dir.push_str(version);
        dir
    };
    std::fs::create_dir_all(&out_dir)?;

    let all_services = stable_svcs.iter().chain(staging_svcs.iter());
    let protocol = Protocol::from_path(protocol_path, stable_svcs, staging_svcs)?;

    let mut builder = Builder::new()
        .write_permissions(perms)
        .modify_prost_config(|mut cfg| {
            cfg.bytes(&[".protocol.batch.v1"]);
            cfg
        });

    for service in all_services.filter(|a| "batch.v1".ne(**a)) {
        builder = builder.modify_hrpc_config(|cfg| {
            cfg.type_attribute(
                format!(".protocol.{}", service),
                "#[derive(::rkyv::Archive, ::rkyv::Serialize, ::rkyv::Deserialize)]",
            )
        });
    }

    builder.generate(protocol, out_dir)?;

    Ok(())
}
