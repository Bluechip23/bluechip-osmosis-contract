use cosmwasm_schema::write_api;
use pool_factory_interfaces::StandardPoolInstantiateMsg;
use standard_pool::msg::{ExecuteMsg, MigrateMsg, QueryMsg};

fn main() {
    write_api! {
        instantiate: StandardPoolInstantiateMsg,
        execute: ExecuteMsg,
        query: QueryMsg,
        migrate: MigrateMsg,
    }
}
