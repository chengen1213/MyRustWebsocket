//! Simple echo websocket server.
//! Open `http://localhost:8080/ws/index.html` in browser
//! or [python console client](https://github.com/actix/examples/blob/master/websocket/websocket-client.py)
//! could be used for testing.

extern crate serde;
extern crate serde_json;
extern crate uuid;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use actix::prelude::*;
use actix_files as fs;
use actix_web::{
    client::{Client, Connector},
    middleware, web, App, Error, HttpRequest, HttpResponse, HttpServer, Result,
};
use actix_web_actors::ws;

use openssl::ssl::{SslAcceptor, SslConnector, SslFiletype, SslMethod};
use uuid::Uuid;

#[macro_use]
extern crate serde_derive;

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

const HOSTNAME: &str = "https://remakeaon.com/";

#[derive(Deserialize)]
struct Action {
    #[serde(rename = "type")]
    file_type: String,
    name: String,
    msg: String,
}

#[derive(Serialize)]
struct Download {
    download: String,
}

fn create_file(name: &String) -> (String, String) {
    let uuid: Uuid = Uuid::new_v4();
    let mut url = "files/".to_owned();
    url.push_str(&uuid.to_string());
    url.push_str("/");
    url.push_str(&name);
    let path = std::path::Path::new(&url);
    let prefix = path.parent().unwrap();
    let display = path.display();
    std::fs::create_dir_all(prefix).unwrap();
    match std::fs::File::create(&path) {
        Err(why) => panic!("couldn't create {}: {}", display, why),
        Ok(file) => file,
    };
    return (uuid.to_string(), url);
}

fn save_txt(name: &String, _msg: &String) -> (String, String) {
    let (uuid, path) = create_file(name);

    std::fs::write(&path, _msg).expect("Unable to write file");

    return (uuid, path);
}

async fn save_img(path: String, _msg: String) {
    // let (uuid, path) = create_file(name);

    println!("make a request to {}", _msg);

    let builder = SslConnector::builder(SslMethod::tls()).unwrap();

    let client = Client::build()
        .connector(Connector::new().ssl(builder.build()).finish())
        .finish();

    let _payload = client
        .get(_msg)
        .send()
        .await
        .unwrap()
        .body()
        .limit(20_000_000) // sets max allowable payload size
        .await
        .unwrap();

    println!("recieve {}", _payload.len());

    println!("write to {}", path);
    std::fs::write(&path, _payload).expect("Unable to write file");
    // return (uuid, path);
}

/// do websocket handshake and start `MyWebSocket` actor
async fn ws_index(
    r: HttpRequest,
    stream: web::Payload,
    state: web::Data<Mutex<HashMap<String, String>>>,
) -> Result<HttpResponse, Error> {
    println!("{:?}", r);
    let res = ws::start(MyWebSocket::new(state), &r, stream);
    println!("{:?}", res);
    res
}

async fn download_file(
    req: HttpRequest,
    path: web::Path<(String,)>,
    state: web::Data<Mutex<HashMap<String, String>>>,
) -> HttpResponse {
    println!("{:?}", req);
    let map = state.lock().unwrap();
    let url = map.get(&path.0).unwrap();

    let path = Path::new(url.as_str());
    if !path.exists() {
        return HttpResponse::NotFound().json({});
    }
    let name = path.file_name().unwrap().to_str().unwrap();
    let data = std::fs::read(path).unwrap();
    HttpResponse::Ok()
        .header(
            "Content-Disposition",
            format!("form-data; filename={}", name),
        )
        .body(data)
}

/// websocket connection is long running connection, it easier
/// to handle with an actor
struct MyWebSocket {
    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    hb: Instant,
    state: web::Data<Mutex<HashMap<String, String>>>,
}

impl Actor for MyWebSocket {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start. We start the heartbeat process here.
    fn started(&mut self, ctx: &mut Self::Context) {
        self.hb(ctx);
    }
}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for MyWebSocket {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        // process websocket messages
        println!("WS: {:?}", msg);
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                self.hb = Instant::now();
            }
            Ok(ws::Message::Text(text)) => {
                let action: Action = serde_json::from_str(&text[..]).unwrap();
                // println!("{:?},{:?}", parsed, parsed["type"]);
                let result;
                if &action.file_type == "text" {
                    result = save_txt(&action.name, &action.msg);
                } else if &action.file_type == "pic" {
                    result = create_file(&action.name);

                    ctx.spawn(Box::new(crate::fut::wrap_future(save_img(
                        result.1.clone(),
                        String::from(&action.msg),
                    ))));
                // result = executor::block_on(self.save_img(&action.name, &action.msg));
                } else {
                    ctx.text("Invalid type!");
                    return;
                }
                let mut url = HOSTNAME.to_owned();
                url.push_str(&result.0);
                url.push_str("/");
                let download = Download { download: url };
                let mut map = self.state.lock().unwrap();
                map.insert(result.0, result.1);
                println!("{:?}", map);
                ctx.text(serde_json::to_string(&download).unwrap());
            }
            // Ok(ws::Message::Binary(bin)) => ctx.binary(bin),
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            }
            _ => ctx.stop(),
        }
    }
}

impl MyWebSocket {
    fn new(state: web::Data<Mutex<HashMap<String, String>>>) -> Self {
        Self {
            hb: Instant::now(),
            state,
        }
    }

    /// helper method that sends ping to client every second.
    ///
    /// also this method checks heartbeats from client
    fn hb(&self, ctx: &mut <Self as Actor>::Context) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // check client heartbeats
            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                // heartbeat timed out
                println!("Websocket Client heartbeat failed, disconnecting!");

                // stop actor
                ctx.stop();

                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "actix_server=info,actix_web=info");
    env_logger::init();

    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    builder
        .set_private_key_file("key.pem", SslFiletype::PEM)
        .unwrap();
    builder.set_certificate_chain_file("cert.pem").unwrap();

    let map: HashMap<String, String> = HashMap::new();
    let files = web::Data::new(Mutex::new(map));

    HttpServer::new(move || {
        App::new()
            .app_data(files.clone())
            // enable logger
            .wrap(middleware::Logger::default())
            .service(web::resource("/{file_id}/").route(web::get().to(download_file)))
            // websocket route
            .service(web::resource("/ws/upload/").route(web::get().to(ws_index)))
            // static files
            .service(fs::Files::new("/", "static/").index_file("index.html"))
    })
    // start http server on 127.0.0.1:8080
    .bind("0.0.0.0:80")?
    .run()
    .await
}
