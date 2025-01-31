use futures_util::{
    future::{self, join_all},
    stream::{SelectAll, StreamExt},
    Stream,
};
use std::{collections::HashMap, error::Error, fmt::Display};
use zbus::{fdo::DBusProxy, proxy, zvariant::OwnedValue, Connection};

#[proxy(
    default_path = "/org/mpris/MediaPlayer2",
    interface = "org.mpris.MediaPlayer2.Player"
)]
trait Player {
    #[zbus(property)]
    fn can_control(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    #[zbus(property)]
    fn volume(&self) -> zbus::Result<f64>;
}

#[derive(Debug)]
struct TitleParseError {}

impl Display for TitleParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to parse media title from MPRIS Player")
    }
}

impl Error for TitleParseError {}

impl PlayerProxy<'_> {
    async fn get_title(&self) -> Result<String, TitleParseError> {
        let m = self.metadata().await.map_err(|_| TitleParseError {})?;
        let artists = m
            .get("xesam:artist")
            .unwrap()
            .clone()
            .try_into()
            .map(|a: Vec<String>| a.join(", "));
        let title = TryInto::<String>::try_into(m.get("xesam:title").unwrap().clone());
        match (artists, title) {
            (Ok(a), Ok(t)) => Ok(format!("{} - {}", a, t)),
            (_, Ok(t)) => Ok(t),
            (Ok(a), _) => Ok(a),
            _ => Err(TitleParseError {}),
        }
    }
}

enum PlayerEvent {
    Names,
    Metadata,
    Volume,
}

const SERVICE_PREFIX: &str = "org.mpris.MediaPlayer2.";
#[derive(Default, Clone, Debug)]
struct PlayerData {
    service_name: String,
    title: Option<String>,
    volume: Option<f64>,
}

impl Display for PlayerData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let service_name = self
            .service_name
            .strip_prefix(SERVICE_PREFIX)
            .unwrap_or(self.service_name.as_str());
        match &self.title {
            Some(t) => match self.volume {
                Some(v) => {
                    write!(f, "{}[{}]: {}", service_name, v, t)
                }
                None => {
                    write!(f, "{}: {}", service_name, t)
                }
            },
            None => {
                write!(f, "{}: Nothing", service_name)
            }
        }
    }
}

// Although we use `tokio` here, you can use any async runtime of choice.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let conn = Connection::session().await?;
    let mut combined = init_streams(&conn).await?;
    let mut names = get_player_names(&conn).await?;
    let mut data = get_data(&conn, &names).await?;
    data.iter().for_each(|d| println!("{d}"));
    while let Some(e) = combined.next().await {
        match e {
            PlayerEvent::Names => {
                combined = init_streams(&conn).await?;
                names = get_player_names(&conn).await?;
                data = get_data(&conn, &names).await?;
            }
            _ => {
                data = get_data(&conn, &names).await?;
            }
        }
        data.iter().for_each(|d| println!("{d}"));
    }

    Ok(())
}

async fn init_streams(
    conn: &Connection,
) -> Result<SelectAll<std::pin::Pin<Box<dyn Stream<Item = PlayerEvent> + Send>>>, Box<dyn Error>> {
    let dbus = DBusProxy::new(conn).await?;
    let names = get_player_names(conn).await?;
    let services = join_all(
        names
            .iter()
            .map(|n| async move { PlayerProxy::new(conn, n.clone()).await.unwrap() }),
    )
    .await;
    let mut combined = SelectAll::new();
    combined.push(
        dbus.receive_name_owner_changed()
            .await?
            .filter_map(|s| {
                future::ready(match s.args() {
                    Ok(a) => a
                        .name
                        .starts_with("org.mpris.MediaPlayer2.")
                        .then_some(PlayerEvent::Names),
                    Err(_) => None,
                })
            })
            .boxed(),
    );
    for s in services.iter() {
        combined.push(
            s.receive_metadata_changed()
                .await
                .map(|_| PlayerEvent::Metadata)
                .boxed(),
        );
    }
    for s in services.iter() {
        combined.push(
            s.receive_volume_changed()
                .await
                .map(|_| PlayerEvent::Volume)
                .boxed(),
        );
    }
    Ok(combined)
}

async fn get_data(conn: &Connection, names: &[String]) -> Result<Vec<PlayerData>, Box<dyn Error>> {
    Ok(join_all(names.iter().map(|n| async {
        let dbus = PlayerProxy::new(conn, n.clone()).await;
        match dbus {
            Ok(player) => {
                let t = player.get_title().await.ok();
                let v = player.volume().await.ok();
                PlayerData {
                    service_name: n.clone(),
                    title: t,
                    volume: v,
                }
            }
            Err(_) => PlayerData {
                service_name: n.clone(),
                ..Default::default()
            },
        }
    }))
    .await)
}

async fn get_player_names(conn: &Connection) -> Result<Vec<String>, Box<dyn Error>> {
    let dbus = DBusProxy::new(conn).await?;
    Ok(dbus
        .list_names()
        .await?
        .iter()
        .filter(|&n| n.starts_with(SERVICE_PREFIX))
        .map(|n| n.to_string())
        .collect())
}
