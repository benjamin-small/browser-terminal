//! Host-defined structured redirection. Targets have no built-in file semantics.

use crate::registry::LocalBoxFuture;
use crate::{ExecContext, ShellError, Value};

pub trait RedirectHandler {
    fn read(&self, target: String, ctx: ExecContext) -> LocalBoxFuture<Result<Value, ShellError>>;
    fn write(
        &self,
        target: String,
        value: Value,
        append: bool,
        ctx: ExecContext,
    ) -> LocalBoxFuture<Result<(), ShellError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{eval_to_value, execute_line, Engine};
    use crate::eval::block_on;
    use crate::registry::ready;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Store {
        value: Value,
        reads: Cell<usize>,
        writes: RefCell<Vec<(String, Value, bool)>>,
        fail_read: Cell<bool>,
        fail_write: Cell<bool>,
    }

    impl RedirectHandler for Store {
        fn read(
            &self,
            _target: String,
            _ctx: ExecContext,
        ) -> LocalBoxFuture<Result<Value, ShellError>> {
            self.reads.set(self.reads.get() + 1);
            ready(if self.fail_read.get() {
                Err(ShellError::runtime("read failed"))
            } else {
                Ok(self.value.clone())
            })
        }

        fn write(
            &self,
            target: String,
            value: Value,
            append: bool,
            _ctx: ExecContext,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            if self.fail_write.get() {
                return ready(Err(ShellError::runtime("write failed")));
            }
            self.writes.borrow_mut().push((target, value, append));
            ready(Ok(()))
        }
    }

    fn fixture(value: Value) -> (Rc<RefCell<Engine>>, Rc<Store>) {
        let engine = Rc::new(RefCell::new(Engine::new()));
        let store = Rc::new(Store {
            value,
            reads: Cell::new(0),
            writes: RefCell::new(vec![]),
            fail_read: Cell::new(false),
            fail_write: Cell::new(false),
        });
        engine
            .borrow_mut()
            .set_redirect_handler(Some(store.clone()));
        (engine, store)
    }

    fn run(engine: &Rc<RefCell<Engine>>, source: &str) -> Result<Value, ShellError> {
        block_on(eval_to_value(
            engine.clone(),
            0,
            source.into(),
            0,
            Rc::new(crate::sink::NullSink),
        ))
    }

    #[test]
    fn read_values_feed_pipelines_and_writes_preserve_collection_shape() {
        let value = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let (engine, store) = fixture(value);
        engine
            .borrow_mut()
            .set_host_var("target", Value::Str("out name".into()))
            .expect("variable");
        assert_eq!(
            run(&engine, "head 1 < source > $target").expect("redirect"),
            Value::Null
        );
        assert_eq!(
            run(&engine, "length < source > \"$target.count\"").expect("redirect"),
            Value::Null
        );
        assert_eq!(
            *store.writes.borrow(),
            vec![
                ("out name".into(), Value::List(vec![Value::Int(1)]), false),
                ("out name.count".into(), Value::Int(3), false),
            ]
        );
        assert_eq!(
            run(&engine, "length < source").expect("read"),
            Value::Int(3)
        );
    }

    #[test]
    fn redirects_preserve_bytes_records_and_append_intent() {
        let bytes = Value::Bytes(vec![0, 128, 255]);
        let (engine, store) = fixture(bytes.clone());
        run(&engine, "map {|x| $x} < src >> dst").expect("bytes");
        run(&engine, "echo '{\"x\":1}' | from json > dst").expect("record");
        run(&engine, "echo '[]' | from json > empty").expect("empty list");
        assert_eq!(
            *store.writes.borrow(),
            vec![
                ("dst".into(), bytes, true),
                (
                    "dst".into(),
                    Value::record([("x".into(), Value::Int(1))]),
                    false
                ),
                ("empty".into(), Value::List(vec![]), false),
            ]
        );
    }

    #[test]
    fn invalid_targets_and_pipeline_failures_do_not_write() {
        let (engine, store) = fixture(Value::Str("hello".into()));
        assert!(run(&engine, "length < source > 42")
            .expect_err("numeric target")
            .msg
            .contains("must be a string"));
        assert_eq!(store.reads.get(), 0, "validate targets before reading");
        assert!(run(&engine, "missing > dst").is_err());
        assert!(run(&engine, "echo 5 | length > dst").is_err());
        store.fail_read.set(true);
        assert!(run(&engine, "length < source > dst")
            .expect_err("read failure")
            .msg
            .contains("read failed"));
        store.fail_write.set(true);
        let error = run(&engine, "echo hi > dst").expect_err("write failure");
        assert!(error.msg.contains("write failed"));
        assert!(error.span.is_some());
        assert!(store.writes.borrow().is_empty());
    }

    #[test]
    fn redirects_consume_terminal_output_and_can_be_disabled() {
        let (engine, store) = fixture(Value::Null);
        block_on(execute_line(
            engine.clone(),
            0,
            "echo hidden-payload > dst".into(),
            0,
        ));
        let text: String = engine
            .borrow_mut()
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                crate::protocol::EngineEvent::PaneOutput { data, .. } => Some(data),
                _ => None,
            })
            .collect();
        assert!(!text.contains("hidden-payload"));
        assert!(text.contains('❯'));
        assert_eq!(store.writes.borrow().len(), 1);
        engine.borrow_mut().set_redirect_handler(None);
        assert!(run(&engine, "echo blocked > dst").is_err());
        assert_eq!(store.writes.borrow().len(), 1);
    }
}
