use kcp2::{KcpConfig, KcpConnector, KcpListener};
use tokio::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(512, 512)
        .timeout(Duration::from_secs(30));

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(std::string::String::as_str) {
        Some("server") => {
            let listener = KcpListener::bind_with_config("0.0.0.0:12345", config).await?;
            println!("服务器启动在 0.0.0.0:12345");

            loop {
                let (conn, addr) = listener.accept().await?;
                println!("新连接: {} (conv: {})", addr, conn.conv());

                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    loop {
                        match conn.recv(&mut buf).await {
                            Ok(size) if size > 0 => {
                                let msg = String::from_utf8_lossy(&buf[..size]);
                                println!("收到: {msg}");

                                let echo = format!("Echo: {msg}");
                                if let Err(e) = conn.send(echo.as_bytes()).await {
                                    eprintln!("发送错误: {e}");
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("接收错误: {e}");
                                break;
                            }
                        }
                    }
                });
            }
        }
        Some("client") => {
            let connector = KcpConnector::new("127.0.0.1:12345")?
                .with_config(config)
                .conv(1);

            let session = connector.connect().await?;
            let conn = session.connection();
            println!("已连接到服务器");

            let messages = ["Hello", "World", "测试消息", "quit"];
            for msg in &messages {
                conn.send(msg.as_bytes()).await?;
                println!("发送: {msg}");

                let mut buf = vec![0u8; 2048];
                match conn.recv(&mut buf).await {
                    Ok(size) if size > 0 => {
                        let response = String::from_utf8_lossy(&buf[..size]);
                        println!("收到: {response}");
                    }
                    _ => {}
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            println!("客户端完成");
        }
        _ => {
            println!("用法:");
            println!("  服务器: cargo run --example high_level_api server");
            println!("  客户端: cargo run --example high_level_api client");
        }
    }

    Ok(())
}
