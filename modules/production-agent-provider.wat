(component
  (import "open-exchange" (func $open-exchange (param "index" u64) (result s32)))
  (import "publish-callable" (func $publish-callable (param "slot" u64) (result s32)))
  (import "event-count" (func $event-count (result s64)))
  (import "read-event" (func $read-event (param "index" u64) (result s64)))
  (import "exchange" (func $exchange (param "event-index" u64) (param "invocation" u64) (result s64)))
  (core func $open-exchange-core (canon lower (func $open-exchange)))
  (core func $publish-callable-core (canon lower (func $publish-callable)))
  (core func $event-count-core (canon lower (func $event-count)))
  (core func $read-event-core (canon lower (func $read-event)))
  (core func $exchange-core (canon lower (func $exchange)))
  (core module $module
    (import "host" "open-exchange" (func $open-exchange (param i64) (result i32)))
    (import "host" "publish-callable" (func $publish-callable (param i64) (result i32)))
    (import "host" "event-count" (func $event-count (result i64)))
    (import "host" "read-event" (func $read-event (param i64) (result i64)))
    (import "host" "exchange" (func $exchange (param i64 i64) (result i64)))
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
      call $open-exchange
      call $checked
      local.tee $status
      i32.eqz
      if
      else
        local.get $status
        return
      end
      i64.const 10
      call $publish-callable
      call $checked
      local.tee $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        local.get $status
      end)
    (func (export "invoke") (param i64) (param $operation i64) (param $invocation i64)
      (param i64) (result i64)
      (local $count i64)
      (local $index i64)
      (local $value i64)
      local.get $operation
      i64.const 1
      i64.lt_u
      if
        i64.const -4
        return
      end
      local.get $invocation
      i64.eqz
      if
        i64.const -4
        return
      end
      call $event-count
      local.tee $count
      i64.const 0
      i64.lt_s
      if
        local.get $count
        return
      end
      local.get $count
      local.set $index
      block $missing
        loop $scan
          local.get $index
          i64.eqz
          br_if $missing
          local.get $index
          i64.const 1
          i64.sub
          local.tee $index
          call $read-event
          local.tee $value
          i64.const 0
          i64.lt_s
          if
            local.get $value
            return
          end
          local.get $value
          i64.const 56
          i64.shr_u
          i64.const 127
          i64.and
          i64.const 1
          i64.eq
          if
            local.get $index
            local.get $invocation
            call $exchange
            return
          end
          br $scan
        end
      end
      i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "open-exchange" (func $open-exchange-core))
    (export "publish-callable" (func $publish-callable-core))
    (export "event-count" (func $event-count-core))
    (export "read-event" (func $read-event-core))
    (export "exchange" (func $exchange-core)))
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
