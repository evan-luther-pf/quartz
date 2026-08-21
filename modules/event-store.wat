(component
  (import "open-journal" (func $open-journal (param "index" u64) (result s32)))
  (import "open-event-stream" (func $open-event-stream (param "index" u64) (result s32)))
  (import "publish-callable" (func $publish-callable (param "slot" u64) (result s32)))
  (core func $open-journal-core (canon lower (func $open-journal)))
  (core func $open-event-stream-core (canon lower (func $open-event-stream)))
  (core func $publish-callable-core (canon lower (func $publish-callable)))
  (core module $module
    (import "host" "open-journal" (func $open-journal (param i64) (result i32)))
    (import "host" "open-event-stream" (func $open-event-stream (param i64) (result i32)))
    (import "host" "publish-callable" (func $publish-callable (param i64) (result i32)))
    (func $checked (param $status i32) (result i32)
      local.get $status
      i32.eqz
      if (result i32)
        i32.const 0
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "start") (param i64) (result i64) i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      i64.const 0
      call $open-journal
      call $checked
      local.tee $status
      i32.eqz
      if
      else
        local.get $status
        return
      end
      i64.const 0
      call $open-event-stream
      call $checked
      local.tee $status
      i32.eqz
      if
      else
        local.get $status
        return
      end
      i64.const 3
      call $publish-callable
      call $checked
      local.tee $status
      i32.eqz
      if
      else
        local.get $status
        return
      end
      i64.const 4
      call $publish-callable
      call $checked
      local.tee $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        local.get $status
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const 1)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "open-journal" (func $open-journal-core))
    (export "open-event-stream" (func $open-event-stream-core))
    (export "publish-callable" (func $publish-callable-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "invoke") (param "instance" u64) (param "operation" u64)
    (param "arg0" u64) (param "arg1" u64) (result s64)
    (canon lift (core func $instance "invoke")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
