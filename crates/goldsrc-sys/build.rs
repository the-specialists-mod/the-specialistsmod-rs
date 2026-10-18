use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // FWGS_DIR, or the nearest `xash3d-fwgs` directory next to any ancestor of this crate (works
    // from the main checkout and from git worktrees).
    let fwgs = env::var("FWGS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        manifest
            .ancestors()
            .map(|a| a.join("xash3d-fwgs"))
            .find(|p| p.join("common/xash3d_types.h").exists())
            .expect("FWGS headers not found: set FWGS_DIR to an xash3d-fwgs checkout")
    });
    let target = env::var("TARGET").unwrap();

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_args(["common", "engine", "pm_shared", "public", "3rdparty/library_suffix/include"].map(|d| format!("-I{}", fwgs.join(d).display())))
        .clang_arg(format!("--target={target}")) // layouts differ between i686 and amd64: generate per target
        .allowlist_type("(enginefuncs_t|DLL_FUNCTIONS|NEW_DLL_FUNCTIONS|globalvars_t|edict_t|entvars_t|KeyValueData|TraceResult|cvar_t)")
        .allowlist_type("(cl_entity_t|cl_enginefunc_t|playermove_t|usercmd_t|entity_state_t|clientdata_t|weapon_data_t|ref_params_t|local_state_t)")
        .allowlist_var("(INTERFACE_VERSION|NEW_DLL_FUNCTIONS_VERSION|CLDLL_INTERFACE_VERSION)")
        .allowlist_var("(FL_|EF_|MOVETYPE_|SOLID_|DAMAGE_|DEAD_|IN_|MSG_|SVC_|CHAN_|ATTN_|PITCH_|kRender|ENTITY_|PM_|CONTENTS_).*")
        .allowlist_type("ALERT_TYPE")
        // render/HUD: event/efx/triangle/demo APIs (only forward-declared by cdll_int.h)
        .header_contents("render_ext.h", "#include \"r_efx.h\"\n#include \"event_api.h\"\n#include \"event_flags.h\"\n#include \"triangleapi.h\"\n#include \"demo_api.h\"\n#include \"screenfade.h\"\n#include \"studio_event.h\"\n#include \"shake.h\"\n")
        .allowlist_type("(event_args_t|event_api_t|efx_api_t|triangleapi_t|demo_api_t|TEMPENTITY|screenfade_t|dlight_t|pmtrace_t|physent_t|mstudioevent_t|TRICULLSTYLE)")
        .allowlist_var("(FTENT_|TRI_|FEV_|FFADE_|TENTPRIORITY_).*")
        // studio renderer: the engine studio API and the types it passes (the model data itself is
        // parsed from bytes by ts-client, so studio.h isn't needed)
        .header_contents("studio_ext.h", "#include \"com_model.h\"\n#include \"r_studioint.h\"\n")
        .allowlist_type("(engine_studio_api_t|r_studio_interface_t|player_info_t|alight_t|cache_user_t)")
        .allowlist_var("(STUDIO_INTERFACE_VERSION|STUDIO_RENDER|STUDIO_EVENTS)")
        // the server's `Server_GetBlendingInterface` export (same header)
        .allowlist_type("(sv_blending_interface_t|server_studio_api_t)")
        .allowlist_var("SV_BLENDING_INTERFACE_VERSION")
        // the client's net API (`pNetAPI`): the connection status and its address (restart and
        // reconnect, docs/implementation/client-plugins.md §13.5)
        .header_contents("net_ext.h", "#include \"net_api.h\"\n")
        .allowlist_type("(net_api_t|net_status_t)")
        .opaque_type("glpoly2_s") // a flexible array member; only reached through pointers
        .rustified_enum("(ALERT_TYPE|PRINT_TYPE)")
        .derive_default(true)
        .generate()
        .expect("bindgen");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings.write_to_file(out).unwrap();
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-env-changed=FWGS_DIR");
}
