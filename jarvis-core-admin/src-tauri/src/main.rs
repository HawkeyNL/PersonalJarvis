fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|value| value == "--credential-entry")
    {
        if arguments.len() != 3 {
            eprintln!("jarvis-core-admin: credential entry requires one supported provider");
            std::process::exit(2);
        }
        jarvis_core_admin_lib::run_credential_entry(&arguments[2]);
    }
    if arguments.len() == 2 && arguments[1] == "--component-version" {
        println!("{}", jarvis_core_admin_lib::component_version());
        return;
    }
    if arguments.len() == 2 && arguments[1] == "--frontend-mode" {
        println!("{}", jarvis_core_admin_lib::frontend_mode());
        return;
    }
    if jarvis_core_admin_lib::broker_requested() {
        if let Err(error) = jarvis_core_admin_lib::run_broker() {
            eprintln!("jarvis-core-admin broker: {error}");
            std::process::exit(1);
        }
        return;
    }
    jarvis_core_admin_lib::run();
}
