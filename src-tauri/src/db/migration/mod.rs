use sea_orm_migration::prelude::*;

mod completion_rebuild;
mod m20260211_000001_init;
mod m20260219_000001_folder_command;
mod m20260221_000001_folder_is_open;
mod m20260226_000001_agent_setting;
mod m20260227_000001_folder_parent_branch;
mod m20260330_000001_chat_channel;
mod m20260401_000001_chat_channel_sender_context;
mod m20260404_000001_model_provider;
mod m20260406_000001_agent_setting_model_provider;
mod m20260420_000001_opened_tabs;
mod m20260422_000001_folder_sort_order;
mod m20260423_000001_drop_folder_parent_branch;
mod m20260424_000001_folder_color;
mod m20260424_000002_quick_message;
mod m20260513_000001_remote_workspace_connection;
mod m20260518_000001_model_provider_single_type_and_model;
mod m20260522_000001_delegation_columns;
mod m20260607_000001_folder_parent_id;
mod m20260608_000001_conversation_title_locked;
mod m20260610_000001_conversation_pinned_at;
mod m20260611_000001_folder_is_chat;
mod m20260612_000001_conversation_folder_kind;
mod m20260621_000001_automation;
mod m20260630_000001_conversation_parent_id_index;
mod m20260703_000001_chat_channel_thread_binding;
mod m20260716_000001_agent_show_thinking;
mod m20260716_000001_auto_title;
mod m20260716_000001_conversation_awaiting_reply_token;
mod m20260716_000002_folder_last_agent;
mod m20260716_000003_delegation_route_reliability;
mod m20260717_000001_event_driven_delegation_join;
mod m20260717_000001_folder_alias;
mod m20260719_000001_delegation_continuations;
mod m20260719_000002_auto_title_first_prompt_at;
mod m20260720_000001_internal_session_translate_purpose;
mod m20260722_000001_auto_title_job_config_gen;
mod m20260723_000001_delegation_task_runs;
mod m20260724_000001_provisional_orphan_repair;
mod m20260726_000001_delegation_workflows;
mod m20260727_000001_workflow_structural_revision;
mod m20260727_000002_workflow_gate_fingerprints;
mod m20260727_000003_workflow_manifest_v2;
mod m20260730_000001_recovery_authorizations;
mod m20260731_000001_custom_agent;
mod m20260731_000002_custom_agent_skills;
mod m20260731_000003_custom_agent_skills_dir;
mod m20260731_000004_custom_agent_source;
mod m20260801_000001_work_task;
mod m20260801_000002_work_task_p2;
mod m20260801_000003_work_task_template;
mod m20260803_000001_folder_link;
mod m20260803_000001_token_usage;
mod m20260804_000001_completion_protocol_and_run_evidence;
mod m20260804_000002_completion_scope_and_gate_settlement;
mod m20260804_000003_completion_tool_intents_and_restart_link;
mod m20260804_000004_typed_completion_attention;
mod m20260806_000001_platform_design_root_binding;
mod m20260806_000002_final_reviewer_snapshots;
mod m20260806_000003_plan_round_authorization;
mod m20260806_000004_legacy_restart_context;
mod m20260807_000001_work_task_scheduled_at;
mod m20260808_000001_custom_agent_supports_mcp;
mod m20260809_000001_completion_protocol_v2_only;
mod m20260811_000001_simple_workflows;
mod m20260817_000001_delegation_orchestration_bindings;
mod m20260817_000001_work_task_conversation_title;
mod m20260819_000001_turn_generation_stat;
mod m20260827_000001_delegation_dispatch_intent;
#[cfg(test)]
pub(crate) use m20260817_000001_delegation_orchestration_bindings::install_for_historical_completion_fixture;
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260211_000001_init::Migration),
            Box::new(m20260219_000001_folder_command::Migration),
            Box::new(m20260221_000001_folder_is_open::Migration),
            Box::new(m20260226_000001_agent_setting::Migration),
            Box::new(m20260227_000001_folder_parent_branch::Migration),
            Box::new(m20260330_000001_chat_channel::Migration),
            Box::new(m20260401_000001_chat_channel_sender_context::Migration),
            Box::new(m20260404_000001_model_provider::Migration),
            Box::new(m20260406_000001_agent_setting_model_provider::Migration),
            Box::new(m20260420_000001_opened_tabs::Migration),
            Box::new(m20260422_000001_folder_sort_order::Migration),
            Box::new(m20260423_000001_drop_folder_parent_branch::Migration),
            Box::new(m20260424_000001_folder_color::Migration),
            Box::new(m20260424_000002_quick_message::Migration),
            Box::new(m20260513_000001_remote_workspace_connection::Migration),
            Box::new(m20260518_000001_model_provider_single_type_and_model::Migration),
            Box::new(m20260522_000001_delegation_columns::Migration),
            Box::new(m20260607_000001_folder_parent_id::Migration),
            Box::new(m20260608_000001_conversation_title_locked::Migration),
            Box::new(m20260610_000001_conversation_pinned_at::Migration),
            Box::new(m20260611_000001_folder_is_chat::Migration),
            Box::new(m20260612_000001_conversation_folder_kind::Migration),
            Box::new(m20260621_000001_automation::Migration),
            Box::new(m20260630_000001_conversation_parent_id_index::Migration),
            Box::new(m20260703_000001_chat_channel_thread_binding::Migration),
            Box::new(m20260716_000001_conversation_awaiting_reply_token::Migration),
            Box::new(m20260716_000001_agent_show_thinking::Migration),
            Box::new(m20260716_000001_auto_title::Migration),
            Box::new(m20260716_000002_folder_last_agent::Migration),
            Box::new(m20260716_000003_delegation_route_reliability::Migration),
            Box::new(m20260717_000001_event_driven_delegation_join::Migration),
            Box::new(m20260717_000001_folder_alias::Migration),
            Box::new(m20260719_000001_delegation_continuations::Migration),
            Box::new(m20260719_000002_auto_title_first_prompt_at::Migration),
            Box::new(m20260720_000001_internal_session_translate_purpose::Migration),
            Box::new(m20260722_000001_auto_title_job_config_gen::Migration),
            Box::new(m20260723_000001_delegation_task_runs::Migration),
            Box::new(m20260724_000001_provisional_orphan_repair::Migration),
            Box::new(m20260726_000001_delegation_workflows::Migration),
            Box::new(m20260727_000001_workflow_structural_revision::Migration),
            Box::new(m20260727_000002_workflow_gate_fingerprints::Migration),
            Box::new(m20260727_000003_workflow_manifest_v2::Migration),
            Box::new(m20260730_000001_recovery_authorizations::Migration),
            Box::new(m20260731_000001_custom_agent::Migration),
            Box::new(m20260731_000002_custom_agent_skills::Migration),
            Box::new(m20260731_000003_custom_agent_skills_dir::Migration),
            Box::new(m20260731_000004_custom_agent_source::Migration),
            Box::new(m20260801_000001_work_task::Migration),
            Box::new(m20260801_000002_work_task_p2::Migration),
            Box::new(m20260801_000003_work_task_template::Migration),
            Box::new(m20260803_000001_folder_link::Migration),
            Box::new(m20260803_000001_token_usage::Migration),
            Box::new(m20260804_000001_completion_protocol_and_run_evidence::Migration),
            Box::new(m20260804_000002_completion_scope_and_gate_settlement::Migration),
            Box::new(m20260804_000003_completion_tool_intents_and_restart_link::Migration),
            Box::new(m20260804_000004_typed_completion_attention::Migration),
            Box::new(m20260806_000001_platform_design_root_binding::Migration),
            Box::new(m20260806_000002_final_reviewer_snapshots::Migration),
            Box::new(m20260806_000003_plan_round_authorization::Migration),
            Box::new(m20260806_000004_legacy_restart_context::Migration),
            Box::new(m20260807_000001_work_task_scheduled_at::Migration),
            Box::new(m20260808_000001_custom_agent_supports_mcp::Migration),
            Box::new(m20260809_000001_completion_protocol_v2_only::Migration),
            Box::new(m20260811_000001_simple_workflows::Migration),
            Box::new(m20260817_000001_delegation_orchestration_bindings::Migration),
            Box::new(m20260817_000001_work_task_conversation_title::Migration),
            Box::new(m20260819_000001_turn_generation_stat::Migration),
            Box::new(m20260827_000001_delegation_dispatch_intent::Migration),
        ]
    }
}
