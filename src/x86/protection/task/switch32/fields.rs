use super::*;
use crate::x86::protection::access::DescriptorWrite;

fn tss_field(task: TaskRegister, offset: u32) -> Lookup {
    Lookup {
        table: Table::Tss,
        base: task.base,
        limit: task.limit,
        index: offset,
        selector: task.selector,
    }
}

pub(super) fn read_field(
    context: &ExecutionContext,
    image: &impl GuestMemory,
    task: TaskRegister,
    offset: u32,
    width: u32,
) -> Result<u32, Failure> {
    let field = tss_field(task, offset);
    let address = field.address(context, 0, width, Access::Read, SystemPhase::TssRead)?;
    let bytes = image
        .read_system(address, width)
        .map_err(|error| field.access_failure(error, SystemPhase::TssRead))?;
    Ok(u32::from_le_bytes(bytes[..4].try_into().unwrap()))
}

pub(super) enum Write {
    Field {
        field: Lookup,
        address: u64,
        width: u32,
        value: u32,
    },
    Busy(DescriptorWrite),
}

impl Write {
    pub(super) fn field(
        context: &ExecutionContext,
        image: &impl GuestMemory,
        task: TaskRegister,
        offset: u32,
        width: u32,
        value: u32,
    ) -> Result<Self, Failure> {
        let field = tss_field(task, offset);
        let address = field.address(context, 0, width, Access::Write, SystemPhase::TssWrite)?;
        image
            .check_system_write(address, width)
            .map_err(|failure| field.access_failure(failure, SystemPhase::TssWrite))?;
        Ok(Self::Field {
            field,
            address,
            width,
            value,
        })
    }

    pub(super) fn failure(&self, error: AccessFailure) -> Failure {
        match self {
            Self::Field { field, .. } => field.access_failure(error, SystemPhase::TssWrite),
            Self::Busy(write) => write.failure(error),
        }
    }

    pub(super) fn commit(self, image: &mut impl GuestMemory) {
        match self {
            Self::Field {
                address,
                width,
                value,
                ..
            } => image
                .write_system(address, width, u64::from(value))
                .expect("admitted TSS publication"),
            Self::Busy(write) => write.commit(image),
        }
    }
}
