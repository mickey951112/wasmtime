;;! target = "aarch64"
(module
    (func (result i64)
	(i64.const 1)
	(i64.const 0x7fffffffffffffff)
	(i64.add)
    )
)
;;    0:	 fd7bbfa9             	stp	x29, x30, [sp, #-0x10]!
;;    4:	 fd030091             	mov	x29, sp
;;    8:	 fc030091             	mov	x28, sp
;;    c:	 300080d2             	mov	x16, #1
;;   10:	 e00310aa             	mov	x0, x16
;;   14:	 1000f092             	mov	x16, #0x7fffffffffffffff
;;   18:	 0060308b             	add	x0, x0, x16, uxtx
;;   1c:	 fd7bc1a8             	ldp	x29, x30, [sp], #0x10
;;   20:	 c0035fd6             	ret	
