//! AST→SSA variable resolution (Braun et al.) for [`crate::lower::Lowerer`]: read/write a local's
//! current SSA value per block, materializing phis on the fly over sealed/unsealed blocks.
use crate::lower::*;

impl<'a> crate::lower::Lowerer<'a> {
    // ---- Braun SSA construction ----

    pub(super) fn write_variable(&mut self, var: VarId, block: BlockId, val: Val) {
        self.current_def.insert((var, block), val);
    }

    pub(super) fn read_variable(&mut self, var: VarId, block: BlockId) -> Val {
        if let Some(v) = self.current_def.get(&(var, block)) {
            return *v;
        }
        self.read_variable_recursive(var, block)
    }

    fn read_variable_recursive(&mut self, var: VarId, block: BlockId) -> Val {
        let ty = self.var_ty[var as usize];
        let val = if !self.sealed[block as usize] {
            let phi = self.fresh_vreg();
            self.blocks[block as usize].phis.push(Phi {
                dst: phi,
                ty,
                args: Vec::new(),
            });
            self.incomplete[block as usize].push((var, phi));
            Val::Reg(phi)
        } else if self.preds[block as usize].len() == 1 {
            let p = self.preds[block as usize][0];
            self.read_variable(var, p)
        } else {
            let phi = self.fresh_vreg();
            self.blocks[block as usize].phis.push(Phi {
                dst: phi,
                ty,
                args: Vec::new(),
            });
            self.write_variable(var, block, Val::Reg(phi));
            self.add_phi_operands(var, block, phi);
            Val::Reg(phi)
        };
        self.write_variable(var, block, val);
        val
    }

    fn add_phi_operands(&mut self, var: VarId, block: BlockId, phi: Vreg) {
        let preds = self.preds[block as usize].clone();
        let mut args = Vec::with_capacity(preds.len());
        for p in preds {
            let v = self.read_variable(var, p);
            args.push((p, v));
        }
        if let Some(ph) = self.blocks[block as usize]
            .phis
            .iter_mut()
            .find(|ph| ph.dst == phi)
        {
            ph.args = args;
        }
    }

    pub(super) fn seal_block(&mut self, block: BlockId) {
        let pending = std::mem::take(&mut self.incomplete[block as usize]);
        for (var, phi) in pending {
            self.add_phi_operands(var, block, phi);
        }
        self.sealed[block as usize] = true;
    }
}
