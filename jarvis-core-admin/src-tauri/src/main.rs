fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
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
