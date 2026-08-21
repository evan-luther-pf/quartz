(component
  (import "call-provider" (func $call-provider
    (param "slot" u64) (param "operation" u64) (param "arg0" u64) (param "arg1" u64)
    (result s64)))
  (import "resume-event" (func $resume-event
    (param "index" u64) (param "value" u64) (result s32)))
  (import "resume-snapshot" (func $resume-snapshot
    (param "event-index" u64) (param "snapshot-index" u64) (param "value" u64)
    (result s32)))
  (import "event-count" (func $event-count (result s64)))
  (import "read-event" (func $read-event (param "index" u64) (result s64)))
  (core func $call-provider-core (canon lower (func $call-provider)))
  (core func $resume-event-core (canon lower (func $resume-event)))
  (core func $resume-snapshot-core (canon lower (func $resume-snapshot)))
  (core func $event-count-core (canon lower (func $event-count)))
  (core func $read-event-core (canon lower (func $read-event)))
  (core module $module
    (import "host" "call-provider" (func $call-provider (param i64 i64 i64 i64) (result i64)))
    (import "host" "resume-event" (func $resume-event (param i64 i64) (result i32)))
    (import "host" "resume-snapshot" (func $resume-snapshot (param i64 i64 i64) (result i32)))
    (import "host" "event-count" (func $event-count (result i64)))
    (import "host" "read-event" (func $read-event (param i64) (result i64)))
    (func $fact (param $kind i64) (param $turn i64) (param $invocation i64)
      (param $data i64) (result i64)
      local.get $kind
      i64.const 56
      i64.shl
      local.get $turn
      i64.const 48
      i64.shl
      i64.or
      local.get $invocation
      i64.const 32
      i64.shl
      i64.or
      local.get $data
      i64.const 4294967295
      i64.and
      i64.or)
    (func $completed (param $status i32) (result i32)
      local.get $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func $append (param $value i64) (result i32)
      i64.const 0
      local.get $value
      call $resume-event
      call $completed)
    (func (export "start") (param i64) (result i64) i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $count i64)
      (local $value i64)
      (local $kind i64)
      (local $turn i64)
      (local $data i64)
      (local $invocation i64)
      (local $result i64)
      (local $answer i64)
      (local $snapshot i64)
      call $event-count
      local.tee $count
      i64.const 0
      i64.lt_s
      if
        local.get $count
        i32.wrap_i64
        return
      end
      local.get $count
      i64.eqz
      if
        i32.const 1
        return
      end
      local.get $count
      i64.const 1
      i64.sub
      call $read-event
      local.tee $value
      i64.const 0
      i64.lt_s
      if
        local.get $value
        i32.wrap_i64
        return
      end
      local.get $value
      i64.const 56
      i64.shr_u
      i64.const 255
      i64.and
      local.set $kind
      local.get $value
      i64.const 48
      i64.shr_u
      i64.const 255
      i64.and
      local.set $turn
      local.get $value
      i64.const 4294967295
      i64.and
      local.set $data
      local.get $turn
      i64.const 4
      i64.shl
      local.set $invocation

      local.get $kind
      i64.const 1
      i64.eq
      if
        i64.const 2
        local.get $turn
        local.get $invocation
        i64.const 1
        i64.add
        i64.const 1
        call $fact
        call $append
        return
      end

      local.get $kind
      i64.const 2
      i64.eq
      if
        local.get $data
        i64.const 1
        i64.eq
        if
          i64.const 10
          i64.const 1
          local.get $invocation
          i64.const 1
          i64.add
          i64.const 0
          call $call-provider
          local.tee $result
          i64.const 0
          i64.lt_s
          if
            local.get $result
            i32.wrap_i64
            return
          end
          i64.const 3
          local.get $turn
          local.get $invocation
          i64.const 2
          i64.add
          i64.const 1
          call $fact
          call $append
          return
        end
        i64.const 10
        i64.const 2
        local.get $data
        i64.const 0
        call $call-provider
        local.tee $result
        i64.const 0
        i64.lt_s
        if
          local.get $result
          i32.wrap_i64
          return
        end
        i64.const 5
        local.get $turn
        i64.const 0
        local.get $result
        call $fact
        call $append
        return
      end

      local.get $kind
      i64.const 3
      i64.eq
      if
        i64.const 11
        i64.const 1
        local.get $invocation
        i64.const 2
        i64.add
        local.get $data
        call $call-provider
        local.tee $result
        i64.const 0
        i64.lt_s
        if
          local.get $result
          i32.wrap_i64
          return
        end
        local.get $result
        i64.const 32
        i64.shr_u
        local.set $answer
        local.get $result
        i64.const 4294967295
        i64.and
        local.set $snapshot
        i64.const 4
        local.get $turn
        local.get $invocation
        i64.const 2
        i64.add
        local.get $answer
        call $fact
        local.set $value
        i64.const 0
        local.get $snapshot
        local.get $value
        call $resume-snapshot
        call $completed
        return
      end

      local.get $kind
      i64.const 4
      i64.eq
      if
        i64.const 2
        local.get $turn
        local.get $invocation
        i64.const 3
        i64.add
        local.get $data
        call $fact
        call $append
        return
      end

      local.get $kind
      i64.const 5
      i64.eq
      if
        i64.const 6
        local.get $turn
        i64.const 0
        i64.const 1
        call $fact
        call $append
        return
      end

      local.get $kind
      i64.const 6
      i64.eq
      if
        i64.const 7
        local.get $turn
        i64.const 0
        i64.const 1
        call $fact
        call $append
        return
      end

      i32.const 1)
    (func (export "invoke") (param i64) (param i64) (param i64) (param i64) (result i64)
      i64.const -3)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "call-provider" (func $call-provider-core))
    (export "resume-event" (func $resume-event-core))
    (export "resume-snapshot" (func $resume-snapshot-core))
    (export "event-count" (func $event-count-core))
    (export "read-event" (func $read-event-core)))
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
