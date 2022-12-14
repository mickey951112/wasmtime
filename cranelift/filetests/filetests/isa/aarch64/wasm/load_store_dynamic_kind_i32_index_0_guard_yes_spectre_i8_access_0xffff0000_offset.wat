;;! target = "aarch64"
;;!
;;! settings = ['enable_heap_access_spectre_mitigation=true']
;;!
;;! compile = true
;;!
;;! [globals.vmctx]
;;! type = "i64"
;;! vmctx = true
;;!
;;! [globals.heap_base]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 0, readonly = true }
;;!
;;! [globals.heap_bound]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 8, readonly = true }
;;!
;;! [[heaps]]
;;! base = "heap_base"
;;! min_size = 0x10000
;;! offset_guard_size = 0
;;! index_type = "i32"
;;! style = { kind = "dynamic", bound = "heap_bound" }

;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
;; !!! GENERATED BY 'make-load-store-tests.sh' DO NOT EDIT !!!
;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!

(module
  (memory i32 1)

  (func (export "do_store") (param i32 i32)
    local.get 0
    local.get 1
    i32.store8 offset=0xffff0000)

  (func (export "do_load") (param i32) (result i32)
    local.get 0
    i32.load8_u offset=0xffff0000))

;; function u0:0:
;; block0:
;;   mov w13, w0
;;   movn w12, #65534
;;   adds x14, x13, x12
;;   b.lo 8 ; udf
;;   ldr x15, [x2, #8]
;;   ldr x2, [x2]
;;   add x0, x2, x0, UXTW
;;   movz x13, #65535, LSL #16
;;   add x0, x0, x13
;;   movz x13, #0
;;   subs xzr, x14, x15
;;   csel x0, x13, x0, hi
;;   csdb
;;   strb w1, [x0]
;;   b label1
;; block1:
;;   ret
;;
;; function u0:1:
;; block0:
;;   mov w13, w0
;;   movn w12, #65534
;;   adds x14, x13, x12
;;   b.lo 8 ; udf
;;   ldr x15, [x1, #8]
;;   ldr x1, [x1]
;;   add x0, x1, x0, UXTW
;;   movz x13, #65535, LSL #16
;;   add x0, x0, x13
;;   movz x13, #0
;;   subs xzr, x14, x15
;;   csel x0, x13, x0, hi
;;   csdb
;;   ldrb w0, [x0]
;;   b label1
;; block1:
;;   ret