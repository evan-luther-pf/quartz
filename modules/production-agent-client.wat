(component
  (import "call-provider" (func $call-provider (param "slot" u64) (param "operation" u64)
    (param "arg0" u64) (param "arg1" u64) (result s64)))
  (import "event-count" (func $event-count (result s64)))
  (import "read-event" (func $read-event (param "index" u64) (result s64)))
  (import "resume-snapshot" (func $resume-snapshot (param "event-index" u64)
    (param "snapshot-index" u64) (param "value" u64) (result s32)))
  (core func $call-provider-core (canon lower (func $call-provider)))
  (core func $event-count-core (canon lower (func $event-count)))
  (core func $read-event-core (canon lower (func $read-event)))
  (core func $resume-snapshot-core (canon lower (func $resume-snapshot)))
  (core module $module
    (import "host" "call-provider" (func $call-provider (param i64 i64 i64 i64) (result i64)))
    (import "host" "event-count" (func $event-count (result i64)))
    (import "host" "read-event" (func $read-event (param i64) (result i64)))
    (import "host" "resume-snapshot" (func $resume-snapshot (param i64 i64 i64) (result i32)))
    (global $prompt (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $prompt
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $count i64)
      (local $index i64)
      (local $value i64)
      (local $turn i64)
      (local $status i32)
      call $event-count
      local.tee $count
      i64.const 0
      i64.lt_s
      if
        local.get $count
        i32.wrap_i64
        return
      end
      block $absent
        loop $scan
          local.get $index
          local.get $count
          i64.ge_u
          br_if $absent
          local.get $index
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
          i64.const 127
          i64.and
          i64.const 1
          i64.eq
          local.get $value
          i64.const 48
          i64.shr_u
          i64.const 255
          i64.and
          global.get $prompt
          i64.eq
          i32.and
          if
            i32.const 1
            return
          end
          local.get $index
          i64.const 1
          i64.add
          local.set $index
          br $scan
        end
      end
      i64.const 12
      i64.const 1
      global.get $prompt
      i64.const 0
      call $call-provider
      local.tee $turn
      i64.const 0
      i64.lt_s
      if
        local.get $turn
        i32.wrap_i64
        return
      end
      i64.const 0
      i64.const 0
      i64.const 1
      i64.const 56
      i64.shl
      local.get $turn
      i64.const 48
      i64.shl
      i64.or
      global.get $prompt
      i64.or
      call $resume-snapshot
      local.set $status
      local.get $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "call-provider" (func $call-provider-core))
    (export "event-count" (func $event-count-core))
    (export "read-event" (func $read-event-core))
    (export "resume-snapshot" (func $resume-snapshot-core)))
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
