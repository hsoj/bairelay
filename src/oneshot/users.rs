use crate::camera::DeviceAdmin;
use anyhow::{Context, Result};

use super::output::{Outcome, UserInfo};

#[derive(Debug, Clone, Copy)]
pub enum UserType {
	User,
	Administrator,
}

impl UserType {
	fn level(self) -> u8 {
		match self {
			UserType::User => 0,
			UserType::Administrator => 1,
		}
	}
}

#[derive(Debug, Clone)]
pub enum Action {
	List,
	Add {
		name: String,
		password: String,
		user_type: UserType,
	},
	Password {
		name: String,
		password: String,
	},
	Delete {
		name: String,
	},
}

pub async fn run(cam: &dyn DeviceAdmin, action: Action) -> Result<Outcome> {
	match action {
		Action::List => {
			let list = cam.users().await.context("get_users failed")?;
			let users = list
				.user_list
				.unwrap_or_default()
				.into_iter()
				.map(|u| UserInfo {
					name: u.user_name,
					level: u.user_level,
				})
				.collect();
			Ok(Outcome::UsersList { users })
		}
		Action::Add {
			name,
			password,
			user_type,
		} => {
			cam.add_user(name.clone(), password, user_type.level())
				.await
				.context("add_user failed")?;
			Ok(Outcome::UserChanged {
				name,
				action: "added".into(),
			})
		}
		Action::Password { name, password } => {
			cam.modify_user(name.clone(), password)
				.await
				.context("modify_user failed")?;
			Ok(Outcome::UserChanged {
				name,
				action: "password changed for".into(),
			})
		}
		Action::Delete { name } => {
			cam.delete_user(name.clone())
				.await
				.context("delete_user failed")?;
			Ok(Outcome::UserChanged {
				name,
				action: "deleted".into(),
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc::xml::{User, UserList};
	use crate::baichuan::bc_protocol::Error;

	use crate::fake_camera::FakeCameraBuilder;

	fn user(name: &str, level: u8) -> User {
		User {
			user_name: name.into(),
			user_level: level,
			user_set_state: "none".into(),
			password: None,
			user_id: None,
			login_state: None,
		}
	}

	#[tokio::test]
	async fn user_type_level_mapping() {
		assert_eq!(UserType::User.level(), 0);
		assert_eq!(UserType::Administrator.level(), 1);
	}

	#[tokio::test]
	async fn users_list_maps_records() {
		let fake = FakeCameraBuilder::new()
			.with_users(|| {
				Ok(UserList {
					user_list: Some(vec![user("admin", 1), user("viewer", 0)]),
					..Default::default()
				})
			})
			.build();
		let outcome = run(&*fake, Action::List).await.unwrap();
		let Outcome::UsersList { users } = outcome else {
			panic!("wrong variant");
		};
		assert_eq!(users.len(), 2);
		assert_eq!(users[0].name, "admin");
		assert_eq!(users[0].level, 1);
		assert_eq!(users[1].name, "viewer");
		assert_eq!(users[1].level, 0);
	}

	#[tokio::test]
	async fn users_list_missing_list_is_empty() {
		let fake = FakeCameraBuilder::new()
			.with_users(|| {
				Ok(UserList {
					user_list: None,
					..Default::default()
				})
			})
			.build();
		let outcome = run(&*fake, Action::List).await.unwrap();
		assert_eq!(outcome, Outcome::UsersList { users: vec![] });
	}

	#[tokio::test]
	async fn users_list_error_propagates() {
		let fake = FakeCameraBuilder::new()
			.with_users(|| Err(Error::Other("no")))
			.build();
		let err = run(&*fake, Action::List).await.unwrap_err();
		assert!(format!("{:#}", err).contains("get_users failed"));
	}

	#[tokio::test]
	async fn users_add_records_call_and_returns_added_action() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(
			&*fake,
			Action::Add {
				name: "new".into(),
				password: "pw".into(),
				user_type: UserType::Administrator,
			},
		)
		.await
		.unwrap();
		assert_eq!(
			outcome,
			Outcome::UserChanged {
				name: "new".into(),
				action: "added".into(),
			}
		);
		assert_eq!(
			*fake.calls().add_user.lock().unwrap(),
			vec![("new".to_string(), "pw".to_string(), 1u8)]
		);
	}

	#[tokio::test]
	async fn users_password_records_call() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(
			&*fake,
			Action::Password {
				name: "u".into(),
				password: "p".into(),
			},
		)
		.await
		.unwrap();
		assert_eq!(
			outcome,
			Outcome::UserChanged {
				name: "u".into(),
				action: "password changed for".into(),
			}
		);
		assert_eq!(
			*fake.calls().modify_user.lock().unwrap(),
			vec![("u".to_string(), "p".to_string())]
		);
	}

	#[tokio::test]
	async fn users_delete_records_call() {
		let fake = FakeCameraBuilder::new().build();
		let outcome = run(
			&*fake,
			Action::Delete {
				name: "gone".into(),
			},
		)
		.await
		.unwrap();
		assert_eq!(
			outcome,
			Outcome::UserChanged {
				name: "gone".into(),
				action: "deleted".into(),
			}
		);
		assert_eq!(
			*fake.calls().delete_user.lock().unwrap(),
			vec!["gone".to_string()]
		);
	}
}
