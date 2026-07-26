//! The two rules that reconcile the streaming transport with commands that
//! still return one whole value.
//!
//! `flatten` runs on the way *out* of a collecting producer; `collect` runs
//! on the way *in* to a collecting consumer. Because both live here, the
//! streaming commands only ever see individual items and never flatten.

use crate::chan::{Closed, Receiver, Sender};
use crate::registry::PipelineData;
use crate::value::Value;

/// Send a producer's result downstream as items. A `List` is a batch: its
/// elements go one at a time (one level only). A scalar is one item; `Empty`
/// sends nothing; `Rendered` text is one item, kept whole.
pub async fn flatten(data: PipelineData, tx: &Sender) -> Result<(), Closed> {
    match data {
        PipelineData::Empty => Ok(()),
        PipelineData::Value(Value::List(items)) => {
            // Record that these items *were* a list before anyone can observe
            // them individually. Without it a one-element list arrives
            // downstream looking exactly like a bare value, and `[x] | length`
            // errors instead of answering 1.
            tx.mark_batched();
            for item in items {
                tx.send(PipelineData::Value(item)).await?;
            }
            Ok(())
        }
        PipelineData::Value(v) => tx.send(PipelineData::Value(v)).await,
        PipelineData::Rendered(s) => tx.send(PipelineData::Rendered(s)).await,
    }
}

/// Gather a whole stream back into one value for a collecting command. N
/// items become a `List`; exactly one item stays that value (unwrapped, so
/// `echo 5` is a scalar, not `[5]`); zero items become an empty `List`.
/// `Rendered` items degrade to `Str` via `into_value`, which is where a
/// trusted stream loses its trust on consumption.
pub async fn collect(rx: &mut Receiver) -> PipelineData {
    let mut items: Vec<Value> = Vec::new();
    while let Some(item) = rx.recv().await {
        items.push(item.into_value());
    }
    // A batched stream is a collection however many items survived it, so
    // `grep` matching one row still yields a one-row table. Only an
    // unbatched stream may unwrap, which is what keeps `echo 5` a scalar.
    if rx.is_batched() {
        return PipelineData::Value(Value::List(items));
    }
    match items.len() {
        // An emptied stream is an empty collection, not "no value" -- so a
        // downstream `length`/`map`/`filter` sees `[]` and works on it,
        // rather than seeing Null and erroring. Only the terminal boundary
        // (eval_pipeline's own collector) keeps 0 items as Empty, since there
        // it means "this pipeline produced nothing to render".
        0 => PipelineData::Value(Value::List(Vec::new())),
        1 => PipelineData::Value(items.into_iter().next().unwrap_or(Value::Null)),
        _ => PipelineData::Value(Value::List(items)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chan::channel;
    use crate::eval::block_on;
    use crate::registry::PipelineData;
    use crate::value::Value;

    fn int(n: i64) -> PipelineData {
        PipelineData::Value(Value::Int(n))
    }

    #[test]
    fn flatten_sends_list_elements_as_separate_items() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            let list = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
            flatten(PipelineData::Value(list), &tx).await.expect("flatten");
            drop(tx);
            let mut seen = Vec::new();
            while let Some(item) = rx.recv().await {
                seen.push(item.into_value());
            }
            assert_eq!(seen, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        });
    }

    #[test]
    fn a_one_element_list_survives_the_round_trip_as_a_list() {
        // The bug this guards: flatten sends `[x]` as a single item, which is
        // byte-identical on the wire to a bare `x`, so collect used to unwrap
        // it and `[x] | length` errored with "expects a list or string".
        let (tx, mut rx) = channel(8);
        block_on(async {
            let list = Value::List(vec![Value::Int(7)]);
            flatten(PipelineData::Value(list), &tx).await.expect("flatten");
            drop(tx);
            assert_eq!(
                collect(&mut rx).await,
                PipelineData::Value(Value::List(vec![Value::Int(7)])),
            );
        });
    }

    #[test]
    fn a_lone_scalar_still_collects_unwrapped() {
        // The other half, and why the flag exists rather than collect simply
        // always wrapping: `echo 5` must stay 5, not become [5].
        let (tx, mut rx) = channel(8);
        block_on(async {
            flatten(PipelineData::Value(Value::Int(5)), &tx).await.expect("flatten");
            drop(tx);
            assert_eq!(collect(&mut rx).await, PipelineData::Value(Value::Int(5)));
        });
    }

    #[test]
    fn batching_survives_a_streaming_stage_that_keeps_one_item() {
        // A streaming stage forwards items without ever seeing the list
        // around them. Sharing the flag is what stops `grep pat | length`
        // from erroring on the single-match case.
        let (up_tx, up_rx) = channel(8);
        let (down_tx, mut down_rx) = channel(8);
        block_on(async {
            let list = Value::List(vec![Value::Int(1), Value::Int(2)]);
            flatten(PipelineData::Value(list), &up_tx).await.expect("flatten");
            drop(up_tx);

            down_tx.share_batching_with(&up_rx);
            let mut up_rx = up_rx;
            // Forward only the first item, as a filter matching one row would.
            if let Some(item) = up_rx.recv().await {
                down_tx.send(item).await.expect("send");
            }
            drop(down_tx);

            assert_eq!(
                collect(&mut down_rx).await,
                PipelineData::Value(Value::List(vec![Value::Int(1)])),
            );
        });
    }

    #[test]
    fn flatten_is_one_level_only() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            let nested = Value::List(vec![
                Value::List(vec![Value::Int(1), Value::Int(2)]),
                Value::List(vec![Value::Int(3)]),
            ]);
            flatten(PipelineData::Value(nested), &tx).await.expect("flatten");
            drop(tx);
            let mut count = 0;
            while rx.recv().await.is_some() {
                count += 1;
            }
            assert_eq!(count, 2, "outer list flattens; inner lists stay whole");
        });
    }

    #[test]
    fn flatten_scalar_and_empty() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            flatten(int(5), &tx).await.expect("scalar");
            flatten(PipelineData::Empty, &tx).await.expect("empty");
            drop(tx);
            let mut seen = Vec::new();
            while let Some(item) = rx.recv().await {
                seen.push(item.into_value());
            }
            assert_eq!(seen, vec![Value::Int(5)]);
        });
    }

    #[test]
    fn collect_gathers_items_back() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            tx.send(int(1)).await.expect("send");
            tx.send(int(2)).await.expect("send");
            drop(tx);
            let collected = collect(&mut rx).await;
            assert_eq!(
                collected.into_value(),
                Value::List(vec![Value::Int(1), Value::Int(2)])
            );
        });
    }

    #[test]
    fn collect_single_item_is_not_wrapped() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            tx.send(int(7)).await.expect("send");
            drop(tx);
            assert_eq!(collect(&mut rx).await.into_value(), Value::Int(7));
        });
    }

    #[test]
    fn collect_empty_stream_is_an_empty_list() {
        // Not `Empty`: a stage that empties a stream (e.g. `grep` with no
        // matches) must still look like a zero-length collection to the
        // next collecting command, not "no input at all". Only the
        // pipeline's terminal collector treats 0 items as `Empty`.
        let (tx, mut rx) = channel(8);
        block_on(async {
            drop(tx);
            assert_eq!(collect(&mut rx).await.into_value(), Value::List(vec![]));
        });
    }
}
