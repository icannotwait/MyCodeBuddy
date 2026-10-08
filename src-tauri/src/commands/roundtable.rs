//! Authenticated desktop entry points for the frozen command set.

use roundtable_protocol::{
    ActorContext, ClientIdentity, ClientKind, ErrorCode, OperatorScope, RtResult,
};
use serde_json::Value;

pub(crate) fn operator_actor(kind: ClientKind) -> ActorContext {
    // v1 has one operator; transport authentication establishes this identity.
    ActorContext::from_trusted_entry(
        "00000000-0000-4000-8000-000000000001"
            .parse()
            .expect("operator uuid"),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind,
            session_ref: "authenticated_operator".into(),
        },
    )
}

pub(crate) fn request_body(params: Value) -> RtResult<Value> {
    if let Some(request) = params.get("request") {
        if params.as_object().is_none_or(|object| object.len() != 1) {
            return Err(crate::roundtable::rt_error(
                ErrorCode::InvalidArgument,
                "request_wrapper",
            ));
        }
        Ok(request.clone())
    } else {
        Ok(params)
    }
}

#[cfg(feature = "tauri-runtime")]
use tauri::Manager;

#[cfg(feature = "tauri-runtime")]
macro_rules! commands {
    ($($name:ident),* $(,)?)=>{$(
        #[tauri::command]
        pub async fn $name(
            slot:tauri::State<'_,std::sync::Arc<crate::roundtable::RoundtableSlot>>,
            request:Value,
            window:tauri::WebviewWindow,
        )->RtResult<Value>{
            let service=slot.current().ok_or_else(||crate::roundtable::rt_error(ErrorCode::RuntimeUnavailable,"coordinator_unavailable"))?;
            let operator=operator_actor(ClientKind::Desktop);
            let actor=ActorContext::from_trusted_entry(operator.principal_id(),OperatorScope::SingleOperator,ClientIdentity{kind:ClientKind::Desktop,session_ref:window.label().to_string()});
            let result=service.execute_command(&actor,stringify!($name),request.clone()).await?;
            if stringify!($name)=="roundtable_attach" {
                let attach:roundtable_protocol::AttachRequest=roundtable_protocol::decode_json(&serde_json::to_vec(&request).map_err(|_|crate::roundtable::rt_error(ErrorCode::InvalidArgument,"request"))?,&roundtable_protocol::ParseLimits::suggested_profile())?;
                let key=format!("{}:{}",window.label(),attach.subscription_id);
                let cloned=std::sync::Arc::clone(&service);
                let channel=format!("roundtable://{}",attach.subscription_id);
                let app=window.app_handle().clone();let label=window.label().to_string();
                let mut seq=0;
                crate::roundtable::advance_private_watermark(&mut seq,&result)?;
                let task=tokio::spawn(async move {
                    use tauri::{Emitter,Manager};
                    let mut interval=tokio::time::interval(std::time::Duration::from_secs(1));
                    loop {
                        interval.tick().await;
                        if app.get_webview_window(&label).is_none(){break;}
                        match cloned.execute_command(&actor,"roundtable_get",serde_json::json!({"room_id":attach.room_id})).await {
                            Ok(snapshot)=>{
                                if crate::roundtable::advance_private_watermark(&mut seq,&snapshot).unwrap_or(false) {
                                    if app.emit_to(&label,&channel,&snapshot).is_err(){break;}
                                }
                            }
                            Err(_)=>break,
                        }
                    }
                });
                service.install_subscription(key,task)?;
            } else if stringify!($name)=="roundtable_detach" {
                if let Some(id)=request["subscription_id"].as_str(){if let Some(task)=service.subscription_tasks.lock().expect("subscriptions").remove(&format!("{}:{id}",window.label())){task.abort();}}
            }
            Ok(result)
        }
    )*};
}
#[cfg(feature = "tauri-runtime")]
commands!(
    roundtable_preflight,
    roundtable_create,
    roundtable_update_draft,
    roundtable_start,
    roundtable_get,
    roundtable_list,
    roundtable_pause,
    roundtable_resume,
    roundtable_stop,
    roundtable_interject,
    roundtable_retry_synthesis,
    roundtable_events,
    roundtable_messages,
    roundtable_evidence,
    roundtable_operation,
    roundtable_clone,
    roundtable_attach,
    roundtable_detach
);
