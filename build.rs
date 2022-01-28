use harmony_build::{Builder, Protocol, Result};

fn main() -> Result<()> {
    build_protocol(
        "before_account_kind",
        &["profile.v1", "harmonytypes.v1"],
        &[],
        |builder| {
            builder.modify_hrpc_config(|cfg| {
                cfg.type_attribute(
                    ".protocol.profile.v1.Profile",
                    "#[archive_attr(derive(::bytecheck::CheckBytes))]",
                )
            })
        },
    )?;
    Ok(())
}

fn build_protocol(
    version: &str,
    stable_svcs: &[&str],
    staging_svcs: &[&str],
    f: impl FnOnce(Builder) -> Builder,
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

    let mut builder = f(Builder::new())
        .modify_hrpc_config(|cfg| cfg.build_client(false).build_server(false))
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
