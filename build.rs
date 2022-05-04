use harmony_build::{Builder, Protocol, Result};

fn main() -> Result<()> {
    build_protocol(
        "before_account_kind",
        &["profile.v1", "harmonytypes.v1"],
        &[],
    )?;
    build_protocol(
        "before_proto_v2",
        &["profile.v1", "emote.v1", "chat.v1", "harmonytypes.v1"],
        &[],
    )?;
    Ok(())
}

fn build_protocol(version: &str, stable_svcs: &[&str], staging_svcs: &[&str]) -> Result<()> {
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
        .modify_hrpc_config(|cfg| cfg.build_client(false).build_server(false))
        .modify_prost_config(|mut cfg| {
            cfg.bytes(&[".protocol.batch.v1"]);
            cfg
        });

    for service in all_services {
        builder = builder.modify_hrpc_config(|cfg| {
            cfg.type_attribute(
                format!(".protocol.{}", service),
                "#[derive(::rkyv::Archive, ::rkyv::Serialize, ::rkyv::Deserialize)]",
            )
            .type_attribute(
                format!(".protocol.{}", service),
                "#[archive_attr(derive(::bytecheck::CheckBytes))]",
            )
        });
    }

    builder.generate(protocol, out_dir)?;

    Ok(())
}
