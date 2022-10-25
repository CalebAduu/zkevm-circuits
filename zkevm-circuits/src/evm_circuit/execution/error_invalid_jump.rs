use crate::{
    evm_circuit::{
        execution::ExecutionGadget,
        param::N_BYTES_PROGRAM_COUNTER,
        step::ExecutionState,
        util::{
            common_gadget::{RestoreContextGadget, SameContextGadget},
            constraint_builder::{
                ConstraintBuilder, StepStateTransition,
                Transition::{Delta, Same, To},
            },
            from_bytes,
            math_gadget::{IsEqualGadget, LtGadget},
            not, CachedRegion, Cell, RandomLinearCombination,
        },
        witness::{Block, Call, ExecStep, Transaction},
    },
    table::CallContextFieldTag,
    util::Expr,
};
use eth_types::{evm_types::OpcodeId, Field, ToLittleEndian};
use ethers_core::utils::__serde_json::value;
use gadgets::util::and::expr;
use halo2_proofs::{circuit::Value, plonk::Error};


#[derive(Clone, Debug)]
pub(crate) struct ErrorInvalidJumpGadget<F> {
    opcode: Cell<F>,
    destination: RandomLinearCombination<F, N_BYTES_PROGRAM_COUNTER>,
    code_length: Cell<F>,
    value: Cell<F>,
    is_code: Cell<F>,
    out_of_range: LtGadget<F, N_BYTES_PROGRAM_COUNTER>,
    is_jump_dest: IsEqualGadget<F>,
    restore_context: RestoreContextGadget<F>,
}

impl<F: Field> ExecutionGadget<F> for ErrorInvalidJumpGadget<F> {
    const NAME: &'static str = "ErrorInvalidJump";

    const EXECUTION_STATE: ExecutionState = ExecutionState::ErrorInvalidJump;

    fn configure(cb: &mut ConstraintBuilder<F>) -> Self {
        let destination = cb.query_rlc();
        let opcode = cb.query_cell();
        let value = cb.query_cell();
        let is_code = cb.query_cell();

        // initialize is_jump_dest
        let is_jump_dest = IsEqualGadget::construct(cb, value.expr(), OpcodeId::JUMPDEST.expr());

        // Pop the value from the stack
        cb.stack_pop(destination.expr());

        // look up bytecode length
        let code_length = cb.bytecode_length(cb.curr.state.code_hash.expr());
        let dest_value = from_bytes::expr(&destination.cells);

        let out_of_range = LtGadget::construct(cb, code_length.expr(), dest_value.clone());
        //if not out of range, check `dest` is invalid
        cb.condition(1.expr() - out_of_range.expr(), |cb| {
            // TODO: if not out of range, Lookup real value
            cb.bytecode_lookup(
                cb.curr.state.code_hash.expr(),
                dest_value.clone(),
                is_code.expr(),
                value.expr(),
            );
            cb.require_boolean("is_code is bool type", is_code.expr());
            cb.require_zero(
                "is_code is false or not JUMPDEST",
                is_code.expr() * is_jump_dest.expr(),
            );
        });


        cb.call_context_lookup(false.expr(), None, CallContextFieldTag::IsSuccess, 0.expr());

        // Go to EndTx only when is_root
        let is_to_end_tx = cb.next.execution_state_selector([ExecutionState::EndTx]);
        cb.require_equal(
            "Go to EndTx only when is_root",
            cb.curr.state.is_root.expr(),
            is_to_end_tx,
        );

        // When it's a root call
        cb.condition(cb.curr.state.is_root.expr(), |cb| {
            // Do step state transition
            cb.require_step_state_transition(StepStateTransition {
                call_id: Same,
                rw_counter: Delta(2.expr() + cb.curr.state.reversible_write_counter.expr()),

                ..StepStateTransition::any()
            });
        });

        // When it's an internal call, need to restore caller's state as finishing this
        // call. Restore caller state to next StepState
        let restore_context = cb.condition(1.expr() - cb.curr.state.is_root.expr(), |cb| {
            RestoreContextGadget::construct(cb, 0.expr(), 2.expr(), 0.expr(), 0.expr(), 0.expr())
        });

        Self {
            opcode,
            destination,
            code_length,
            value,
            is_code,
            out_of_range,
            is_jump_dest,
            restore_context,
        }
    }

    fn assign_exec_step(
        &self,
        region: &mut CachedRegion<'_, '_, F>,
        offset: usize,
        block: &Block<F>,
        _: &Transaction,
        call: &Call,
        step: &ExecStep,
    ) -> Result<(), Error> {
        let opcode = step.opcode.unwrap();
        self.opcode
            .assign(region, offset, Value::known(F::from(opcode.as_u64())))?;
        let destination = block.rws[step.rw_indices[0]].stack_value();
        self.destination.assign(
            region,
            offset,
            Some(
                destination.to_le_bytes()[..N_BYTES_PROGRAM_COUNTER]
                    .try_into()
                    .unwrap(),
            ),
        )?;

        let code = block
            .bytecodes
            .get(&call.code_hash)
            .expect("could not find current environment's bytecode");
        let code_length = code.bytes.len() as u64;
        self.code_length
            .assign(region, offset, Value::known(F::from(code_length)))?;

        // set default value in case can not find value, is_code from bytecode table
        let mut code_pair = [0u8, 0u8];
        if destination.as_u64() < code_length {
            // get real value from bytecode table
            code_pair = code.get(destination.as_usize());
        }

        self.value
            .assign(region, offset, Value::known(F::from(code_pair[0] as u64)))?;
        self.is_code
            .assign(region, offset, Value::known(F::from(code_pair[1] as u64)))?;
        self.is_jump_dest.assign(
            region,
            offset,
            F::from(code_pair[0] as u64),
            F::from(OpcodeId::JUMPDEST.as_u64()),
        )?;

        self.out_of_range.assign(
            region,
            offset,
            F::from(code_length),
            F::from(destination.as_u64()),
        )?;
        self.restore_context
            .assign(region, offset, block, call, step, 2)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::{evm_circuit::test::rand_range, test_util::run_test_circuits};
    use eth_types::bytecode;
    use mock::TestContext;

    fn test_invalid_jump(destination: usize, out_of_range:bool) {
        let mut bytecode = bytecode! {
            PUSH32(if out_of_range { destination + 10} else { destination })
            JUMP
        };

        // incorrect assigning for invalid jump
        for _ in 0..(destination - 33) {
            bytecode.write(0, false);
        }
        bytecode.append(&bytecode! {
            JUMPDEST
            STOP
        });

        assert_eq!(
            run_test_circuits(
                TestContext::<2, 1>::simple_ctx_with_bytecode(bytecode).unwrap(),
                None
            ),
            Ok(())
        );
    }

    #[test]
    fn invalid_jump_err() {
        test_invalid_jump(34, false);
    }

    #[test]
    fn invalid_jump_outofrange() {
        test_invalid_jump(40, true);
    }
    // TODO: add internal call test
}
