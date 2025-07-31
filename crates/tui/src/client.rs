use crate::common::ClientOptions;
use std::cell::UnsafeCell;

pub type ClientId = usize;

enum ClientInner {
    Vacant(ClientOptions),
    Occupied(reqwest::Client),
}

pub struct Client(UnsafeCell<ClientInner>);

impl Client {
    pub(crate) fn get(&self) -> reqwest::Client {
        let inner = unsafe { &mut *self.0.get() };
        match inner {
            ClientInner::Vacant(options) => {
                let client = reqwest::Client::builder()
                    .default_headers(options.headers())
                    .build()
                    .unwrap();
                *inner = ClientInner::Occupied(client.clone());
                client
            }
            ClientInner::Occupied(client) => client.clone(),
        }
    }

    pub fn new(options: ClientOptions) -> Client {
        Client(UnsafeCell::new(ClientInner::Vacant(options)))
    }
}
