use crate::{
    evm_circuit::{
        execution::ExecutionGadget,
        step::ExecutionState,
        util::{
            common_gadget::SameContextGadget,
            constraint_builder::{EVMConstraintBuilder, StepStateTransition, Transition::Delta},
            from_bytes, CachedRegion,
        },
        witness::{Block, Call, ExecStep, Transaction},
    },
    table::BlockContextFieldTag,
    util::{
        word::{WordCell, WordExpr},
        Expr,
    },
};
use bus_mapping::evm::OpcodeId;
use eth_types::Field;
use halo2_proofs::plonk::Error;
use itertools::chain;

#[derive(Clone, Debug)]
pub(crate) struct ChainIdGadget<F> {
    same_context: SameContextGadget<F>,
    chain_id: WordCell<F>,
}

impl<F: Field> ExecutionGadget<F> for ChainIdGadget<F> {
    const NAME: &'static str = "CHAINID";

    const EXECUTION_STATE: ExecutionState = ExecutionState::CHAINID;

    fn configure(cb: &mut EVMConstraintBuilder<F>) -> Self {
        let chain_id = cb.query_word_unchecked();

        // Push the value to the stack
        cb.stack_push(chain_id.to_word());

        // Lookup block table with chain_id
        cb.block_lookup(
            BlockContextFieldTag::ChainId.expr(),
            None,
            //chain_id.to_word(),
            chain_id.lo().expr(),
        );

        // State transition
        let opcode = cb.query_cell();
        let step_state_transition = StepStateTransition {
            rw_counter: Delta(1.expr()),
            program_counter: Delta(1.expr()),
            stack_pointer: Delta((-1).expr()),
            gas_left: Delta(-OpcodeId::CHAINID.constant_gas_cost().expr()),
            ..Default::default()
        };
        let same_context = SameContextGadget::construct(cb, opcode, step_state_transition);

        Self {
            same_context,
            chain_id,
        }
    }

    fn assign_exec_step(
        &self,
        region: &mut CachedRegion<'_, '_, F>,
        offset: usize,
        block: &Block<F>,
        _: &Transaction,
        _: &Call,
        step: &ExecStep,
    ) -> Result<(), Error> {
        self.same_context.assign_exec_step(region, offset, step)?;

        let chain_id = block.rws[step.rw_indices[0]].stack_value();
        self.chain_id.assign_u256(region, offset, chain_id)?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::test_util::CircuitTestBuilder;
    use eth_types::bytecode;
    use mock::test_ctx::TestContext;

    #[test]
    fn chainid_gadget_test() {
        let bytecode = bytecode! {
            #[start]
            CHAINID
            STOP
        };

        CircuitTestBuilder::new_from_test_ctx(
            TestContext::<2, 1>::simple_ctx_with_bytecode(bytecode).unwrap(),
        )
        .run();
    }
}
