use niri_ipc::{socket::Socket, Request, Action};
fn main() {
    let mut socket = Socket::connect().unwrap();
    socket.send(Request::Action(Action::FocusColumnFirst {})).unwrap();
    socket.send(Request::Action(Action::FocusColumnLast {})).unwrap();
}
