use axum::extract::{ws::WebSocket, Path, State};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::ops::Deref;
use std::sync::atomic::AtomicUsize;
use std::{collections::HashMap, sync::Arc};

use axum::{extract::ws::Message as WSMessage, response::IntoResponse};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::Sender;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Location {
    latitude: f64,
    longitude: f64,
}

impl From<Location> for geoutils::Location {
    fn from(v: Location) -> Self {
        Self::new(v.latitude, v.longitude)
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SharedTaxi {
    location: Location,
}

impl SharedTaxi {
    pub fn should_send(&self, user: &User) -> bool {
        let userloc: geoutils::Location = user.location().into();
        let selfloc: geoutils::Location = self.location.clone().into();

        userloc
            .is_in_circle(&selfloc, geoutils::Distance::from_meters(1000))
            .unwrap_or(false)
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Customer {
    location: Location,
}

impl Customer {
    pub fn should_send(&self, user: &User) -> bool {
        let userloc: geoutils::Location = user.location().into();
        let selfloc: geoutils::Location = self.location.clone().into();

        userloc
            .is_in_circle(&selfloc, geoutils::Distance::from_meters(1000))
            .unwrap_or(false)
        // match user {
        //     User::Driver(_) => true,
        //     User::Customer(_) => false,
        // }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Bus {
    location: Location,
}

impl Bus {
    pub fn should_send(&self, user: &User) -> bool {
        let userloc: geoutils::Location = user.location().into();
        let selfloc: geoutils::Location = self.location.clone().into();

        userloc
            .is_in_circle(&selfloc, geoutils::Distance::from_meters(1000))
            .unwrap_or(false)
    }
}

#[derive(Serialize, Clone, Debug)]
pub enum User {
    Customer(Customer),
    SharedTaxi(SharedTaxi),
    Bus(Bus),
}

impl From<SharedTaxi> for User {
    fn from(v: SharedTaxi) -> Self {
        Self::SharedTaxi(v)
    }
}

impl From<Customer> for User {
    fn from(v: Customer) -> Self {
        Self::Customer(v)
    }
}

impl From<Bus> for User {
    fn from(v: Bus) -> Self {
        Self::Bus(v)
    }
}

impl User {
    pub fn location(&self) -> Location {
        match self {
            User::SharedTaxi(it) => it.location.clone(),
            User::Customer(it) => it.location.clone(),
            User::Bus(it) => it.location.clone(),
        }
    }

    pub fn set_location(&mut self, location: Location) {
        match self {
            User::SharedTaxi(it) => it.location = location,
            User::Customer(it) => it.location = location,
            User::Bus(it) => it.location = location,
        }
    }

    pub fn should_send(&self, user: &User) -> bool {
        match self {
            User::SharedTaxi(it) => it.should_send(user),
            User::Customer(it) => it.should_send(user),
            User::Bus(it) => it.should_send(user),
        }
    }
}

// message that is send by all handler
#[derive(Debug, Clone)]
pub enum StateMessage {
    NewUser(String),
    UpdateLocation(String),
    CloseUser(String),
}

pub struct Inner {
    sender: Sender<StateMessage>,
    list: Mutex<HashMap<String, Option<User>>>,
    last_id: AtomicUsize,
}

impl Default for Inner {
    fn default() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(10);
        Self {
            sender,
            list: Default::default(),
            last_id: Default::default(),
        }
    }
}

#[derive(Clone, Default)]
struct RouteState(Arc<Inner>);

impl Deref for RouteState {
    type Target = Arc<Inner>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Default)]
pub struct GeoStateInner {
    shared_taxi: Mutex<HashMap<String, RouteState>>,
    bus: Mutex<HashMap<String, RouteState>>,
}

#[derive(Default, Clone)]
pub struct GeoState(Arc<GeoStateInner>);

impl RouteState {
    pub fn insert_user(&self, user: Option<User>) -> String {
        let mut guard = self.0.list.lock();
        loop {
            let id = self
                .0
                .last_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .to_string();
            // let id = nanoid::nanoid!();
            if !guard.contains_key(&id) {
                guard.insert(id.clone(), user);
                break id;
            }
        }
    }
}

pub async fn customer_shared_taxi(
    Path(name): Path<String>,
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<GeoState>,
) -> impl IntoResponse {
    let state = state.0.shared_taxi.lock().entry(name).or_default().clone();
    ws.on_upgrade(move |ws| async move { handle_websocket::<Customer>(state, ws).await })
}

pub async fn shared_taxi(
    Path(name): Path<String>,
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<GeoState>,
) -> impl IntoResponse {
    let state = state.0.shared_taxi.lock().entry(name).or_default().clone();
    ws.on_upgrade(move |ws| async move { handle_websocket::<SharedTaxi>(state, ws).await })
}

pub async fn customer_bus(
    Path(name): Path<String>,
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<GeoState>,
) -> impl IntoResponse {
    let state = state.0.bus.lock().entry(name).or_default().clone();
    ws.on_upgrade(move |ws| async move { handle_websocket::<Customer>(state, ws).await })
}

pub async fn bus(
    Path(name): Path<String>,
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<GeoState>,
) -> impl IntoResponse {
    let state = state.0.bus.lock().entry(name).or_default().clone();
    ws.on_upgrade(move |ws| async move { handle_websocket::<Bus>(state, ws).await })
}

#[derive(Debug)]
pub enum DMessage {
    // message that is send by other handler
    StateMessage(StateMessage),
    // message that is send by client
    WSMessage(WSMessage),
}

#[derive(Serialize, Deserialize)]
pub struct UpdateLocation {
    location: Location,
}

// message that user receive
#[derive(Serialize, Debug)]
pub enum UserReceiveMessage {
    NewUser { id: String, user: User },
    UpdateLocation { id: String, location: Location },
    RemoveUser { id: String },
}

// message that user send
#[derive(Deserialize)]
pub enum UserSendMessage<U> {
    InitialMessage(U),
    UpdateLocation { location: Location },
}

pub struct UserState {
    id: String,
    state: RouteState,
    sink: SplitSink<WebSocket, WSMessage>,
    users: HashSet<String>,
}

impl UserState {
    fn should_send_id(&self, id: &String) -> Option<User> {
        let users = self.state.0.list.lock();
        let user = users.get(id).map(|it| it.as_ref()).flatten();
        let current = users.get(&self.id).map(|it| it.as_ref()).flatten();

        if let (Some(current), Some(user)) = (current, user) {
            if current.should_send(&user) {
                Some(user.clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    async fn send_force(&mut self, msg: UserReceiveMessage) -> Result<(), axum::Error> {
        tracing::trace!("sending to {} {:?}", self.id, msg);
        let it = match serde_json::to_string(&msg) {
            Ok(it) => it,
            Err(err) => {
                tracing::error!("{}", err);
                return Ok(());
            }
        };

        self.sink.send(WSMessage::Text(it)).await
    }

    pub async fn send_message(&mut self, message: StateMessage) -> bool {
        match message {
            StateMessage::NewUser(new_id) => {
                if new_id == self.id {
                    return true;
                }

                if let Some(user) = self.should_send_id(&new_id) {
                    tracing::trace!("insert {new_id} in {}", self.id);
                    self.users.insert(new_id.clone());

                    self.send_force(UserReceiveMessage::NewUser { id: new_id, user })
                        .await
                        .ok();
                }
            }
            StateMessage::UpdateLocation(new_id) => {
                if new_id == self.id {
                    return true;
                }

                if self.users.contains(&new_id) {
                    if let Some(user) = self.should_send_id(&new_id) {
                        self.send_force(UserReceiveMessage::UpdateLocation {
                            id: new_id,
                            location: user.location().clone(),
                        })
                        .await
                        .ok();
                    } else {
                        tracing::trace!("remove {new_id} in {}", self.id);
                        self.users.remove(&new_id);

                        self.send_force(UserReceiveMessage::RemoveUser { id: new_id })
                            .await
                            .ok();
                    }
                } else {
                    if let Some(user) = self.should_send_id(&new_id) {
                        tracing::trace!("insert {new_id} in {}", self.id);
                        self.users.insert(new_id.to_string());

                        self.send_force(UserReceiveMessage::NewUser { id: new_id, user })
                            .await
                            .ok();
                    }
                }
            }
            StateMessage::CloseUser(new_id) => {
                if new_id == self.id {
                    return false;
                }

                if self.users.contains(&new_id) {
                    tracing::trace!("remove {new_id} in {}", self.id);
                    self.users.remove(&new_id);

                    self.send_force(UserReceiveMessage::RemoveUser { id: new_id })
                        .await
                        .ok();
                }
            }
        };

        true
    }
}

async fn handle_websocket<U>(state: RouteState, ws: WebSocket)
where
    U: Into<User> + DeserializeOwned,
{
    let current_id = state.insert_user(None);
    tracing::debug!("new user {current_id}");

    let (tx, rx) = async_channel::unbounded();
    let (wsender, mut wrecv) = ws.split();

    let mut userstate = UserState {
        id: current_id.to_string(),
        sink: wsender,
        state: state.clone(),
        users: Default::default(),
    };

    let state_receiver = async {
        let mut receiver = state.sender.subscribe();

        while let Ok(it) = receiver.recv().await {
            if tx.send(DMessage::StateMessage(it)).await.is_err() {
                break;
            }
        }
    };

    let state_sender = state.0.sender.clone();

    let close_user = || {
        state.0.list.lock().remove(&current_id);
        state_sender
            .send(StateMessage::CloseUser(current_id.clone()))
            .ok();
        tracing::info!("closing user {current_id}");
    };

    let websocket_sender = async {
        while let Some(Ok(it)) = wrecv.next().await {
            if tx.send(DMessage::WSMessage(it)).await.is_err() {
                break;
            }
        }

        tracing::trace!("closing websocket {current_id}");
        close_user();
    };

    let channel_receiver = async {
        while let Ok(it) = rx.recv().await {
            tracing::trace!("received: {current_id} {it:?}");
            match it {
                DMessage::StateMessage(message) => {
                    if !userstate.send_message(message).await {
                        break;
                    }
                }
                DMessage::WSMessage(msg) => match msg {
                    WSMessage::Text(text) => {
                        let message: UserSendMessage<U> = match serde_json::from_str(&text) {
                            Ok(it) => it,
                            Err(err) => {
                                tracing::error!("{}", err);
                                continue;
                            }
                        };

                        let send_result = match message {
                            UserSendMessage::InitialMessage(request) => {
                                match state.0.list.lock().get_mut(&current_id) {
                                    Some(it) => {
                                        if it.is_some() {
                                            continue;
                                        }

                                        let user = request.into();
                                        *it = Some(user);
                                        drop(it);
                                    }
                                    None => continue,
                                }

                                let users = state.0.list.lock().clone();
                                for (id, user) in users {
                                    if user.is_some() {
                                        userstate.send_message(StateMessage::NewUser(id)).await;
                                    }
                                }

                                Some(state_sender.send(StateMessage::NewUser(current_id.clone())))
                            }
                            UserSendMessage::UpdateLocation { location } => {
                                match state
                                    .list
                                    .lock()
                                    .get_mut(&current_id)
                                    .and_then(|it| it.as_mut())
                                {
                                    Some(it) => it.set_location(location),
                                    None => continue,
                                };

                                Some(
                                    state_sender
                                        .send(StateMessage::UpdateLocation(current_id.clone())),
                                )
                            }
                        };

                        if let Some(Err(err)) = send_result {
                            tracing::error!("{}", err);
                        }
                    }
                    WSMessage::Close(_) => {
                        break;
                    }
                    _ => {}
                },
            }
        }
    };

    futures::join!(state_receiver, websocket_sender, channel_receiver);

    close_user();

    state.0.list.lock().remove(&current_id);
}
