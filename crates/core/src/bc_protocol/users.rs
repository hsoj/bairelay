use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};
use zeroize::Zeroizing;

#[cfg(test)]
use crate::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Returns all users configured in the camera. Captured Argus XML
	/// advertises `security/user_rw`; gate matches that key. `add_user`,
	/// `modify_user`, `delete_user` all flow through `get_users` first
	/// (read-modify-write) before the stricter `_rw` gate in
	/// `set_users` — failure-mode is ro vs rw cleanly distinguished.
	pub async fn get_users(&self) -> Result<UserList> {
		self.has_ability_ro("user").await?;
		let connection = self.get_connection();

		let msg_num = self.new_message_num();
		let mut sub_get = connection
			.subscribe(MSG_ID_GET_ABILITY_SUPPORT, msg_num)
			.await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_GET_ABILITY_SUPPORT,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: Some(Extension {
					user_name: Some("admin".to_owned()),
					..Default::default()
				}),
				payload: None,
			}),
		};

		sub_get.send(get).await?;
		let msg = sub_get.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		// Valid message with response_code == 200
		if let BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(BcXml {
				user_list: Some(user_list),
				..
			})),
			..
		}) = msg.body
		{
			Ok(user_list)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected ModernMsg payload with a user_list but it was not received",
			})
		}
	}

	/// Add a new user.
	///
	/// `password` is wrapped in [`Zeroizing`] so the local copy is wiped
	/// on every early-return / scope-exit path. The downstream `User`
	/// struct + XML serialisation still hold plaintext copies on the
	/// wire path; a fully end-to-end zeroized pipe would require typed
	/// `Zeroizing<String>` on the trait + serializer paths and is out
	/// of scope here.
	pub async fn add_user(
		&self,
		user_name: String,
		password: String,
		user_level: u8,
	) -> Result<()> {
		let password = Zeroizing::new(password);

		let mut users = self.get_users().await?.user_list.unwrap_or_default();
		if users.iter().any(|user| user.user_name == user_name) {
			return Err(Error::UserAlreadyExists { user_name });
		}

		users.push(User {
			user_set_state: "add".to_owned(),
			user_name,
			password: Some((*password).clone()),
			user_level,
			user_id: None,
			login_state: None,
		});

		self.set_users(users).await
	}

	/// Modify a user. It seems the only property of a user that is
	/// modifiable is the password.
	pub async fn modify_user(&self, user_name: String, password: String) -> Result<()> {
		let password = Zeroizing::new(password);

		let mut users = self.get_users().await?.user_list.unwrap_or_default();

		if let Some(user) = users.iter_mut().find(|user| user.user_name == user_name) {
			user.user_set_state = "modify".to_owned();
			user.password = Some((*password).clone());
		} else {
			return Err(Error::UserNotFound { user_name });
		}

		self.set_users(users).await
	}

	/// Remove a user. Returns [`Error::UserNotFound`] if `user_name` is
	/// not in the camera's user list.
	pub async fn delete_user(&self, user_name: String) -> Result<()> {
		let mut users = self.get_users().await?.user_list.unwrap_or_default();

		if let Some(user) = users.iter_mut().find(|user| user.user_name == user_name) {
			user.user_set_state = "delete".to_owned();
		} else {
			return Err(Error::UserNotFound { user_name });
		}

		self.set_users(users).await
	}

	/// Helper method to send a UserList and wait for its
	/// success/failure. Centralised `_rw` gate for add/modify/delete.
	async fn set_users(&self, users: Vec<User>) -> Result<()> {
		self.has_ability_rw("user").await?;
		let bcxml = BcXml {
			user_list: Some(UserList {
				version: "1.1".to_owned(),
				user_list: Some(users),
			}),
			..Default::default()
		};

		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_set = connection
			.subscribe(MSG_ID_UPDATE_USER_LIST, msg_num)
			.await?;

		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_UPDATE_USER_LIST,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: Some(BcPayloads::BcXml(bcxml)),
			}),
		};

		sub_set.send(get).await?;
		super::set_helpers::await_set_reply_with_quirk(
			&mut sub_set,
			super::set_helpers::SET_QUIRK_TIMEOUT,
		)
		.await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_users_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(vec![User {
								user_set_state: "none".to_string(),
								user_name: "admin".to_string(),
								password: None,
								user_level: 1,
								user_id: Some(1),
								login_state: Some(1),
							}]),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let list = cam.get_users().await.expect("ok");
		let users = list.user_list.expect("some users");
		assert_eq!(users.len(), 1);
		assert_eq!(users[0].user_name, "admin");
	}

	#[tokio::test]
	async fn get_users_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let err = cam.get_users().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_users_missing_xml_returns_err() {
		// 200 OK but no user_list -> UnintelligibleReply.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let err = cam.get_users().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	/// Mock `get_users` then a set_users reply (either 200 or error).
	/// Installs the `user` ability so tests reach the wire mock instead
	/// of short-circuiting on the gate added 2026-05-01.
	async fn mock_get_then_set_users(existing: Vec<User>, set_reply: fn(&Bc) -> Bc) -> BcCamera {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(move |req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(existing),
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_UPDATE_USER_LIST)
			.reply_with(set_reply)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		cam
	}

	/// As `mock_get_then_set_users` but runs an inspector on the SET
	/// request payload before answering with `reply_200_empty`. Used
	/// by happy-path tests to pin the wire-shape of the
	/// add / modify / delete round trip.
	async fn mock_get_then_set_users_inspect<F>(existing: Vec<User>, inspect: F) -> BcCamera
	where
		F: FnOnce(&BcXml) + Send + Sync + 'static,
	{
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(move |req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(existing),
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_UPDATE_USER_LIST)
			.reply_with_xml(move |req, xml| {
				inspect(xml);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		cam
	}

	fn make_user(name: &str) -> User {
		User {
			user_set_state: "none".to_string(),
			user_name: name.to_string(),
			password: None,
			user_level: 1,
			user_id: Some(1),
			login_state: Some(1),
		}
	}

	#[tokio::test]
	async fn add_user_happy_path() {
		// Pin the wire-shape of the SET request: the existing admin
		// user must be echoed verbatim and the new "guest" entry
		// appended with user_set_state="add", password="pw",
		// user_level=0. A regression that flipped the state string
		// (e.g. "add" → "modify") would silently change semantics.
		let cam = mock_get_then_set_users_inspect(vec![make_user("admin")], |xml| {
			let list = xml.user_list.as_ref().expect("user_list on SET request");
			let users = list
				.user_list
				.as_ref()
				.expect("user_list.user_list populated on SET");
			assert_eq!(users.len(), 2, "must include existing + new user");
			let guest = users
				.iter()
				.find(|u| u.user_name == "guest")
				.expect("guest");
			assert_eq!(guest.user_set_state, "add");
			assert_eq!(guest.password.as_deref(), Some("pw"));
			assert_eq!(guest.user_level, 0);
		})
		.await;
		cam.add_user("guest".to_string(), "pw".to_string(), 0)
			.await
			.expect("ok");
	}

	#[tokio::test]
	async fn add_user_existing_returns_err() {
		// No set is attempted because add_user short-circuits once it
		// sees the user exists.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(vec![make_user("admin")]),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let err = cam
			.add_user("admin".to_string(), "pw".to_string(), 0)
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::UserAlreadyExists { ref user_name } if user_name == "admin"));
	}

	#[tokio::test]
	async fn add_user_set_non_200_returns_err() {
		let cam =
			mock_get_then_set_users(vec![make_user("admin")], |req| reply_err_code(req, 500)).await;
		let err = cam
			.add_user("guest".to_string(), "pw".to_string(), 0)
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn add_user_get_fails_propagates() {
		// get_users fails → add_user reports the same error before any
		// user-existence check.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let err = cam
			.add_user("guest".to_string(), "pw".to_string(), 0)
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn modify_user_happy_path() {
		let cam = mock_get_then_set_users_inspect(vec![make_user("admin")], |xml| {
			let users = xml
				.user_list
				.as_ref()
				.and_then(|l| l.user_list.as_ref())
				.expect("user_list on SET");
			assert_eq!(users.len(), 1);
			let admin = &users[0];
			assert_eq!(admin.user_name, "admin");
			assert_eq!(admin.user_set_state, "modify");
			assert_eq!(admin.password.as_deref(), Some("new_pw"));
		})
		.await;
		cam.modify_user("admin".to_string(), "new_pw".to_string())
			.await
			.expect("ok");
	}

	#[tokio::test]
	async fn modify_user_not_found_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(vec![make_user("admin")]),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let err = cam
			.modify_user("nobody".to_string(), "pw".to_string())
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::UserNotFound { ref user_name } if user_name == "nobody"));
	}

	#[tokio::test]
	async fn delete_user_happy_path() {
		let cam = mock_get_then_set_users_inspect(vec![make_user("admin")], |xml| {
			let users = xml
				.user_list
				.as_ref()
				.and_then(|l| l.user_list.as_ref())
				.expect("user_list on SET");
			assert_eq!(users.len(), 1, "delete preserves the row, marks state");
			let admin = &users[0];
			assert_eq!(admin.user_name, "admin");
			// The TODO flagged delete_user: it MUST send
			// user_set_state="delete", not "remove" or anything else.
			// A regression that mis-mapped the wire string would
			// silently leave the user in place.
			assert_eq!(admin.user_set_state, "delete");
		})
		.await;
		cam.delete_user("admin".to_string()).await.expect("ok");
	}

	#[tokio::test]
	async fn delete_user_not_found_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(vec![make_user("admin")]),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		let err = cam
			.delete_user("nobody".to_string())
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::UserNotFound { ref user_name } if user_name == "nobody"));
	}

	#[tokio::test]
	async fn delete_user_set_non_200_returns_err() {
		let cam =
			mock_get_then_set_users(vec![make_user("admin")], |req| reply_err_code(req, 500)).await;
		let err = cam
			.delete_user("admin".to_string())
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn delete_user_no_reply_returns_ok() {
		// set_users wraps recv in a 500 ms timeout; no-reply → Ok.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_ABILITY_SUPPORT)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						user_list: Some(UserList {
							version: "1.1".to_string(),
							user_list: Some(vec![make_user("admin")]),
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_UPDATE_USER_LIST)
			.reply_none()
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("user", true).await;
		cam.delete_user("admin".to_string())
			.await
			.expect("no-reply path returns Ok");
	}

	#[tokio::test]
	async fn get_users_without_ability_returns_missing_ability() {
		// Empty-abilities camera must short-circuit on the gate before
		// any wire I/O — proves the gate added 2026-05-01 fires.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_users().await.expect_err("must require ability");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn add_user_when_user_list_field_absent() {
		// UserList::user_list is Option<Vec<User>>; when None, add_user
		// seeds with `unwrap_or(Vec::new())` — the SET request must
		// then carry exactly the new user.
		let cam = mock_get_then_set_users_inspect(vec![], |xml| {
			let users = xml
				.user_list
				.as_ref()
				.and_then(|l| l.user_list.as_ref())
				.expect("user_list on SET");
			assert_eq!(users.len(), 1);
			assert_eq!(users[0].user_name, "alice");
			assert_eq!(users[0].user_set_state, "add");
		})
		.await;
		cam.add_user("alice".to_string(), "pw".to_string(), 0)
			.await
			.expect("ok");
	}
}
