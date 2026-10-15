(module
  (memory (export "mem") 1)
  ;; n dynamic-length memory.copy calls of len bytes
  (func (export "bulk_copy") (param $n i32) (param $len i32)
    (loop $l
      (memory.copy (i32.const 1024) (i32.add (i32.const 0) (i32.and (local.get $n) (i32.const 7))) (local.get $len))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br_if $l (local.get $n))))
  ;; n dynamic-length memory.fill calls of len bytes
  (func (export "bulk_fill") (param $n i32) (param $len i32)
    (loop $l
      (memory.fill (i32.const 1024) (local.get $n) (local.get $len))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br_if $l (local.get $n))))
  ;; the same copies as a byte loop
  (func (export "loop_copy") (param $n i32) (param $len i32)
    (local $i i32)
    (loop $l
      (local.set $i (i32.const 0))
      (block $done
        (loop $b
          (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
          (i32.store8 (i32.add (i32.const 1024) (local.get $i))
            (i32.load8_u (i32.add (i32.and (local.get $n) (i32.const 7)) (local.get $i))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $b)))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      (br_if $l (local.get $n))))
)
