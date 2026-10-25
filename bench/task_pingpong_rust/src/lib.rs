// the two halves of the ping-pong pair shared by the three task_pingpong
// binaries. tokio's bounded mpsc channel with capacity 1 stands in for
// pith's Channel[Int](1); each channel has one receiver, so mpsc is enough.

use tokio::sync::mpsc::{Receiver, Sender};

// echoes value+1 until it is told to stop with a negative token.
pub async fn ponger(mut req: Receiver<i64>, rsp: Sender<i64>) -> i64 {
    let mut seen = 0;
    while let Some(value) = req.recv().await {
        if value < 0 {
            break;
        }
        rsp.send(value + 1).await.unwrap();
        seen += 1;
    }
    seen
}

pub async fn pinger(req: Sender<i64>, mut rsp: Receiver<i64>, rounds: i64) -> i64 {
    let mut sum = 0;
    for i in 0..rounds {
        req.send(i).await.unwrap();
        if let Some(back) = rsp.recv().await {
            sum += back;
        }
    }
    req.send(-1).await.unwrap();
    sum
}

// the argument at `index` when it parses as a positive integer.
pub fn positive_arg(index: usize, fallback: i64) -> i64 {
    std::env::args()
        .nth(index)
        .and_then(|a| a.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(fallback)
}
